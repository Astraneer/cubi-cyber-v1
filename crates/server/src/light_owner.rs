//! Derived LIGHT shares the canonical WAL group and its publication barrier.
use super::*;
use crate::lighting::{LightingPhase, ServerLighting, ServerLightingConfig};
mod durable;

pub(super) struct LightOwner {
    engine: ServerLighting,
    pending: Option<LightingPhase>,
    columns: BTreeMap<(SpaceId, i32, i32), BTreeSet<PageId>>,
    sky_pending: BTreeSet<PageId>,
    sky_ready: bool,
    sourced: BTreeSet<(i32, i32)>,
    source_pending: VecDeque<(i32, i32)>,
    sections_revision: u64,
    sections_stamp: [u8; 32],
    durable_ops: Vec<u8>,
}
impl LightOwner {
    pub(super) fn is_idle(&self) -> bool {
        self.pending.is_none()
            && self.sky_pending.is_empty()
            && self.source_pending.is_empty()
            && self.engine.is_idle()
    }
    pub(super) fn attach(
        world: &mut World,
        budget: world::lighting::LightBudget,
    ) -> Result<Self, ServerError> {
        let engine = ServerLighting::attach_durable(
            world,
            ServerLightingConfig {
                max_bootstrap_pages: 65_536,
                step: budget,
                ..ServerLightingConfig::default()
            },
        )
        .map_err(|_| ServerError::Invalid("authority lighting initialization"))?;
        let mut columns: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        for (page, _) in world.pages() {
            columns
                .entry((page.space, page.x, page.z))
                .or_default()
                .insert(page);
        }
        Ok(Self {
            engine,
            pending: None,
            columns,
            sky_pending: BTreeSet::new(),
            sky_ready: false,
            sourced: BTreeSet::new(),
            source_pending: VecDeque::new(),
            sections_revision: 0,
            sections_stamp: [0; 32],
            durable_ops: Vec::new(),
        })
    }
    pub(super) fn acknowledge(&mut self, world: &mut World) -> Result<(), ServerError> {
        if let Some(phase) = self.pending.take() {
            self.engine
                .acknowledge_after_durable(world, &phase)
                .map_err(|_| ServerError::Invalid("authority lighting durable boundary"))?;
        }
        Ok(())
    }
}
impl Server {
    fn source_light_profile(&mut self) {
        if self.generation.is_none() {
            return;
        }
        if self.lighting.sections_revision != self.runtimes.sections_revision() {
            self.lighting.sections_revision = self.runtimes.sections_revision();
            let columns: BTreeSet<_> = self
                .runtimes
                .active_sections()
                .filter(|p| p.space == SpaceId(0))
                .map(|p| (p.x, p.z))
                .collect();
            let mut hash = blake3::Hasher::new();
            hash.update(b"cubi lighting active columns v1");
            for (x, z) in &columns {
                hash.update(&x.to_le_bytes());
                hash.update(&z.to_le_bytes());
            }
            let stamp = *hash.finalize().as_bytes();
            if self.lighting.sections_stamp != stamp {
                let removed: Vec<_> = self
                    .lighting
                    .sourced
                    .iter()
                    .filter(|c| !columns.contains(c))
                    .copied()
                    .collect();
                for column in removed {
                    self.lighting.sourced.remove(&column);
                    self.lighting.record_column(5, column);
                }
                self.lighting.source_pending = columns
                    .into_iter()
                    .filter(|c| !self.lighting.sourced.contains(c))
                    .collect();
                self.lighting.sections_stamp = stamp;
                self.lighting.record_sources();
            }
        }
        let Some((x, z)) = self.lighting.source_pending.front().copied() else {
            return;
        };
        // Exact canonical pages provide the known vertical chain down from the
        // certified profile boundary. Never use terrain height as a sky proof.
        for y in 0..8 {
            self.request_generation(PageId {
                space: SpaceId(0),
                x,
                y,
                z,
            });
        }
        let generation = self.generation.as_ref().expect("enabled generation");
        let queued = generation.requested.contains_key(&(x, z))
            || (0..8).all(|y| {
                let page = PageId {
                    space: SpaceId(0),
                    x,
                    y,
                    z,
                };
                self.world.page(page).is_some() || generation.ready.iter().any(|(p, _)| *p == page)
            });
        if queued {
            self.lighting.source_pending.pop_front();
            self.lighting.sourced.insert((x, z));
            self.lighting.record_column(6, (x, z));
            self.counters.light_profile_columns += 1;
        }
    }
    pub(super) fn tick_lighting(&mut self, batch: &mut Batch) -> Result<(), ServerError> {
        self.lighting.observe_changes(&self.world, &batch.changes)?;
        self.source_light_profile();
        let plan = self
            .runtimes
            .height
            .plan_batch(&batch.changes)
            .map_err(|_| ServerError::Invalid("height edit admission/capacity"))?;
        if let Some(plan) = &plan {
            batch.runtimes.push(save::RuntimeRecord {
                kind: save::RUNTIME_HEIGHT,
                payload: plan
                    .encode()
                    .map_err(|_| ServerError::Invalid("height delta bounds"))?,
            });
        }
        let ready = self.height_ready();
        if ready && !self.lighting.sky_ready {
            let pages: Vec<_> = self
                .lighting
                .columns
                .values()
                .flat_map(|p| p.iter().copied())
                .collect();
            for page in pages {
                self.lighting.queue_sky(page);
            }
        }
        if self.lighting.sky_ready != ready {
            self.lighting.sky_ready = ready;
            self.lighting
                .durable_ops
                .extend_from_slice(&[1, u8::from(ready)]);
        }
        if let Some(plan) = &plan {
            for edit in plan.edits().iter().filter(|e| e.position.y > 127) {
                let page = edit.position.page();
                if let Some(pages) = self.lighting.columns.get(&(page.space, page.x, page.z)) {
                    let pages: Vec<_> = pages.iter().copied().collect();
                    for page in pages {
                        self.lighting.queue_sky(page);
                    }
                }
            }
        }
        for _ in 0..4 {
            let Some(page) = self.lighting.sky_pending.first().copied() else {
                break;
            };
            if !self.lighting.engine.request_sky_page(page, self.tick) {
                break;
            }
            self.lighting.sky_pending.remove(&page);
            self.lighting.record_page(3, page);
        }
        // The immutable certificate overlays this not-yet-durable batch, so a
        // newly placed roof darkens the SAME group; missing migration stays dark.
        let sky = self
            .runtimes
            .height
            .sky_proof(SpaceId(0), plan.as_ref())
            .map_err(|_| ServerError::Invalid("height sky certificate"))?;
        let certified = |position: BlockPos| {
            if ready {
                world::lighting::SkyAccess::incoming_from_unknown_above(&sky, position)
            } else {
                None
            }
        };
        let mut phase = self
            .lighting
            .engine
            .step(&mut self.world, self.tick, &certified)
            .map_err(|error| ServerError::Invalid(error.diagnostic()))?;
        self.counters.light_pending_cells = phase.progress.pending_cells;
        self.counters.light_pending_seed_pages = phase.progress.pending_seed_pages;
        self.counters.light_pending_bootstrap_pages = phase.bootstrap_pending;
        self.counters.light_pending_sky_pages = self.lighting.sky_pending.len();
        self.counters.light_sky_pages_queued += phase.work.sky_pages_queued;
        self.counters.light_input_recoveries += phase.work.input_recoveries;
        self.counters.light_recovery_page_probes += phase.work.recovery_page_probes;
        self.counters.light_cells += phase.progress.work.cells_examined;
        self.counters.light_neighbor_reads += phase.progress.work.neighbor_reads;
        self.counters.light_updates += phase.progress.work.light_updates;
        if !phase.changes.is_empty() {
            batch
                .changes
                .extend(phase.changes.drain(..).map(|c| (*c).clone()));
            self.lighting.pending = Some(phase);
        }
        Ok(())
    }
}
