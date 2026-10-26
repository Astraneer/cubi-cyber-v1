//! Full GI source bakes have bounded immutable snapshots and worker results.
//! No full-page classification is permitted in the presentation owner.
use super::*;
use crate::gi_source::{BuiltGiSource, GI_RETAINED_BYTES, GiBakeStamp, GiBakeTask};
use jobs::{Completion, Dispatch, Finish, JobId, JobOutcome, JobSpec, Priority, Scheduler, Take};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::atomic::{AtomicU32, Ordering},
};

const MAX_JOBS: usize = 4;
const MAX_AGE: u64 = 40;
const JOB_KIND: u16 = 0x4749;
const EPOCH_LIMIT: u32 = (1 << 21) - 1;
static NEXT_EPOCH: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Debug, Default)]
pub struct SourceJobStats {
    pub submitted: u64,
    pub completed: u64,
    pub stale: u64,
    pub failed: u64,
    pub worker_reads: u64,
    pub worker_owners: u64,
    pub wire_reads: u64,
    pub wire_bytes: u64,
    pub out_of_scope: u64,
    pub active: usize,
    pub ready: usize,
    pub retained_bytes: usize,
}

struct Context {
    stamp: GiBakeStamp,
    born: Tick,
}
pub(super) struct Ready {
    pub stamp: GiBakeStamp,
    pub built: BuiltGiSource,
}
pub(super) struct SourceJobs {
    scheduler: Scheduler<GiBakeTask>,
    contexts: BTreeMap<JobId, Context>,
    ready: VecDeque<Ready>,
    epoch: u32,
    next_id: u32,
    pub stats: SourceJobStats,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) native: Option<jobs::NativePool<GiBakeTask, BuiltGiSource>>,
}
impl SourceJobs {
    pub fn new() -> Result<Self, ViewEnvironmentError> {
        let scheduler = Scheduler::new(jobs::Limits {
            max_jobs: MAX_JOBS,
            max_bytes: MAX_JOBS * GI_RETAINED_BYTES,
            max_age_ticks: MAX_AGE,
            reserved_interactive_jobs: 1,
            reserved_interactive_bytes: GI_RETAINED_BYTES,
        })
        .map_err(|_| ViewEnvironmentError::InvalidConfig)?;
        let epoch = NEXT_EPOCH
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < EPOCH_LIMIT).then_some(n + 1)
            })
            .map_err(|_| ViewEnvironmentError::InvalidConfig)?;
        Ok(Self {
            scheduler,
            contexts: BTreeMap::new(),
            ready: VecDeque::with_capacity(MAX_JOBS),
            epoch,
            next_id: 1,
            stats: SourceJobStats::default(),
            #[cfg(not(target_arch = "wasm32"))]
            native: Some(
                crate::gi_source::gi_native_pool(2, 2)
                    .map_err(|e| ViewEnvironmentError::Worker(format!("GI workers: {e:?}")))?,
            ),
        })
    }
    fn wire_id(&self, id: JobId) -> JobId {
        JobId((u64::from(self.epoch) << 32) | id.0)
    }
    fn local_id(&self, id: JobId) -> Option<JobId> {
        (id.0 >> 32 == u64::from(self.epoch)).then_some(JobId(id.0 & u64::from(u32::MAX)))
    }
    pub fn busy(&self, page: PageId) -> bool {
        self.contexts.values().any(|c| c.stamp.page == page)
            || self.ready.iter().any(|c| c.stamp.page == page)
    }
    pub fn pending(&self) -> bool {
        !self.contexts.is_empty() || !self.ready.is_empty()
    }
    pub fn capacity(&self) -> bool {
        self.contexts.len() + self.ready.len() < MAX_JOBS
    }
    fn refresh_stats(&mut self) {
        self.stats.active = self.contexts.len();
        self.stats.ready = self.ready.len();
        // Deliberately retain the worst-case snapshot+result allowance until publication.
        self.stats.retained_bytes = (self.stats.active + self.stats.ready) * GI_RETAINED_BYTES;
    }
    pub fn enqueue(
        &mut self,
        builder: &GiSourceBuilder,
        scene: &ClientScene,
        page: PageId,
        source_epoch: u64,
        edited: bool,
        tick: Tick,
    ) -> Result<bool, ViewEnvironmentError> {
        if self.busy(page) || !self.capacity() {
            return Ok(false);
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ViewEnvironmentError::InvalidConfig)?;
        let task = builder.capture_full_optional(
            scene.world(),
            page,
            scene.cached_biome_tints(page).map(|t| Box::new(*t)),
            source_epoch,
            (u64::from(self.epoch) << 32) | u64::from(id),
        )?;
        let stamp = task.stamp().clone();
        let spec = JobSpec::derived(
            jobs::JobKey {
                page,
                kind: JOB_KIND,
            },
            if edited {
                Priority::Interactive
            } else {
                Priority::Normal
            },
            jobs::WorkCost {
                cells: 28672,
                bytes: GI_RETAINED_BYTES as u64,
                ..jobs::WorkCost::ZERO
            },
            GI_RETAINED_BYTES,
            tick,
        );
        match self.scheduler.enqueue(spec, task) {
            Ok(id) => {
                self.contexts.insert(id, Context { stamp, born: tick });
                self.stats.submitted += 1;
                self.refresh_stats();
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }
    pub fn cancel_stale(
        &mut self,
        builder: &GiSourceBuilder,
        world: &world::World,
        epoch: u64,
        tick: Tick,
        contains: impl Fn(PageId) -> bool,
    ) {
        // Four fixed slots: no temporary collection or unbounded scan.
        let mut ids = [None; MAX_JOBS];
        let mut count = 0;
        for (&id, c) in &self.contexts {
            if !contains(c.stamp.page)
                || tick.0.saturating_sub(c.born.0) > MAX_AGE
                || !builder.full_current(world, &c.stamp, epoch)
            {
                ids[count] = Some(id);
                count += 1;
            }
        }
        for id in ids.into_iter().flatten() {
            if self.scheduler.cancel(id).is_some() {
                self.contexts.remove(&id);
                self.stats.stale += 1;
            }
        }
        self.ready.retain(|r| {
            let current = contains(r.stamp.page) && builder.full_current(world, &r.stamp, epoch);
            if !current {
                self.stats.stale += 1;
            }
            current
        });
        self.refresh_stats();
    }
    pub fn take_task(
        &mut self,
        tick: Tick,
    ) -> Result<Option<Dispatch<GiBakeTask>>, ViewEnvironmentError> {
        let mut budget = jobs::WorkBudget::unrestricted(jobs::WorkCost {
            cells: 28672,
            bytes: GI_RETAINED_BYTES as u64,
            ..jobs::WorkCost::ZERO
        });
        loop {
            match self.scheduler.take_next(tick, &mut budget, |_| None) {
                Take::Ready(mut d) => {
                    d.id = self.wire_id(d.id);
                    return Ok(Some(d));
                }
                Take::Expired(e) => {
                    self.contexts.remove(&e.id);
                    self.stats.stale += 1;
                    self.refresh_stats();
                }
                Take::Empty | Take::BudgetBlocked => return Ok(None),
            }
        }
    }
    pub fn requeue(
        &mut self,
        mut dispatch: Dispatch<GiBakeTask>,
    ) -> Result<(), ViewEnvironmentError> {
        let Some(id) = self.local_id(dispatch.id) else {
            self.stats.out_of_scope += 1;
            return Ok(());
        };
        dispatch.id = id;
        if let Err(d) = self.scheduler.requeue(dispatch) {
            self.complete_local(Completion {
                id: d.id,
                outcome: JobOutcome::Cancelled,
            })?;
        }
        Ok(())
    }
    fn complete_local(
        &mut self,
        completion: Completion<BuiltGiSource>,
    ) -> Result<(), ViewEnvironmentError> {
        // Count executed work even when its certificate subsequently fails.
        if let JobOutcome::Success(built) = &completion.outcome {
            self.stats.completed += 1;
            self.stats.worker_reads += built.work.world_reads;
            if built.result.is_ok() {
                self.stats.worker_owners += 4096;
            }
        }
        let context = self.contexts.remove(&completion.id);
        match self.scheduler.finish(completion, |_| None) {
            Finish::Published(built) => {
                let Some(context) = context else {
                    self.stats.out_of_scope += 1;
                    return Ok(());
                };
                if context.stamp.id != built.id {
                    return Err(ViewEnvironmentError::Worker(
                        "GI response belongs to a different captured task".into(),
                    ));
                }
                if built.result.is_err() {
                    self.stats.failed += 1;
                }
                if self.ready.len() >= MAX_JOBS {
                    return Err(ViewEnvironmentError::InvalidConfig);
                }
                self.ready.push_back(Ready {
                    stamp: context.stamp,
                    built,
                });
            }
            Finish::UnknownJob => self.stats.out_of_scope += 1,
            Finish::Failed(_) => self.stats.failed += 1,
            _ => self.stats.stale += 1,
        }
        self.refresh_stats();
        Ok(())
    }
    pub fn complete(
        &mut self,
        mut completion: Completion<BuiltGiSource>,
    ) -> Result<(), ViewEnvironmentError> {
        let Some(id) = self.local_id(completion.id) else {
            self.stats.out_of_scope += 1;
            return Ok(());
        };
        completion.id = id;
        self.complete_local(completion)
    }
    pub fn complete_bytes(&mut self, id: JobId, bytes: &[u8]) -> Result<(), ViewEnvironmentError> {
        match BuiltGiSource::decode(bytes) {
            Ok(built) => self.complete(Completion {
                id,
                outcome: JobOutcome::Success(built),
            }),
            Err(e) => {
                self.fail(id)?;
                Err(ViewEnvironmentError::Worker(format!("GI response: {e:?}")))
            }
        }
    }
    pub fn fail(&mut self, id: JobId) -> Result<(), ViewEnvironmentError> {
        self.complete(Completion {
            id,
            outcome: JobOutcome::Failed(jobs::JobFailure::WorkerDisconnected),
        })
    }
    pub fn encode(&mut self, d: &Dispatch<GiBakeTask>) -> Result<Vec<u8>, ViewEnvironmentError> {
        if self.local_id(d.id).is_none() {
            return Err(ViewEnvironmentError::InvalidConfig);
        }
        let bytes = d
            .payload
            .encode()
            .map_err(|e| ViewEnvironmentError::Worker(format!("GI request: {e:?}")))?;
        self.stats.wire_reads += d.payload.wire_samples();
        self.stats.wire_bytes += bytes.len() as u64;
        Ok(bytes)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn poll_native(&mut self) -> Result<(), ViewEnvironmentError> {
        for _ in 0..MAX_JOBS {
            let Some(c) = self.native.as_ref().and_then(|native| native.try_recv()) else {
                break;
            };
            self.complete(c)?;
        }
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn dispatch_native(&mut self, tick: Tick) -> Result<(), ViewEnvironmentError> {
        if self.native.is_none() {
            return Ok(());
        }
        for _ in 0..2 {
            let Some(d) = self.take_task(tick)? else {
                break;
            };
            match self
                .native
                .as_ref()
                .expect("prepared worker pool")
                .try_submit(d)
            {
                Ok(()) => {}
                Err(jobs::SubmitError::Full(d)) => {
                    self.requeue(d)?;
                    break;
                }
                Err(jobs::SubmitError::Closed(d)) => {
                    self.fail(d.id)?;
                    return Err(ViewEnvironmentError::Worker(
                        "GI workers disconnected".into(),
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn pop_ready(&mut self) -> Option<Ready> {
        let r = self.ready.pop_front();
        self.refresh_stats();
        r
    }
    pub fn push_ready(&mut self, r: Ready) {
        self.ready.push_front(r);
        self.refresh_stats();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SceneConfig, gi_source::execute_gi_request};
    use core_types::Cell;
    #[test]
    fn bounded_worker_contexts_reserve_interactive_slot_and_reject_other_session() {
        let atlas = AtlasImage {
            tile_size: 1,
            layers: 186,
            mips: vec![render::terrain_gpu::AtlasMip {
                size: 1,
                rgba: vec![255; 186 * 4],
                tint_mask: vec![255; 186],
            }],
        };
        let mut builder = GiSourceBuilder::new(GiAlbedo::from_atlas(&atlas).unwrap(), 8).unwrap();
        builder.set_sky_ambient([0.; 3]).unwrap();
        builder.enable_directional();
        let mut scene = ClientScene::new(SceneConfig {
            horizontal_radius: 0,
            vertical_radius: 0,
            ..Default::default()
        })
        .unwrap();
        let mut tx = world::Transaction::new(Tick(1), SpaceId(0));
        for x in 0..5 {
            tx.create_page(
                PageId {
                    space: SpaceId(0),
                    x,
                    y: 0,
                    z: 0,
                },
                vec![Cell::AIR; 4096],
            );
        }
        scene.derived_world_mut().commit(tx).unwrap();
        let page = |x| PageId {
            space: SpaceId(0),
            x,
            y: 0,
            z: 0,
        };
        let mut jobs = SourceJobs::new().unwrap();
        #[cfg(not(target_arch = "wasm32"))]
        jobs.native.take();
        for x in 0..3 {
            assert!(
                jobs.enqueue(&builder, &scene, page(x), 0, false, Tick(1))
                    .unwrap()
            );
        }
        assert!(
            !jobs
                .enqueue(&builder, &scene, page(3), 0, false, Tick(1))
                .unwrap(),
            "normal work preserves one edit slot"
        );
        assert!(
            jobs.enqueue(&builder, &scene, page(3), 0, true, Tick(1))
                .unwrap()
        );
        assert!(
            !jobs
                .enqueue(&builder, &scene, page(4), 0, true, Tick(1))
                .unwrap()
        );
        assert_eq!(jobs.stats.retained_bytes, 4 * GI_RETAINED_BYTES);
        while let Some(d) = jobs.take_task(Tick(1)).unwrap() {
            let bytes = jobs.encode(&d).unwrap();
            let response = execute_gi_request(&bytes, || false).unwrap();
            jobs.complete_bytes(JobId(d.id.0 ^ (1 << 32)), &response)
                .unwrap();
            jobs.complete_bytes(d.id, &response).unwrap();
        }
        assert_eq!(
            (jobs.stats.active, jobs.stats.ready, jobs.stats.out_of_scope),
            (0, 4, 4)
        );
        assert_eq!(jobs.stats.worker_reads, 4 * 4096);
        assert_eq!(
            jobs.stats.retained_bytes,
            4 * GI_RETAINED_BYTES,
            "ready results retain their reservation"
        );
        assert!(
            !jobs
                .enqueue(&builder, &scene, page(4), 0, true, Tick(1))
                .unwrap()
        );
        jobs.cancel_stale(&builder, scene.world(), 1, Tick(2), |_| true);
        assert_eq!(
            (
                jobs.stats.ready,
                jobs.stats.retained_bytes,
                jobs.stats.stale
            ),
            (0, 0, 4)
        );
        assert!(
            jobs.enqueue(&builder, &scene, page(4), 1, true, Tick(2))
                .unwrap()
        );
        let d = jobs.take_task(Tick(2)).unwrap().unwrap();
        let request = jobs.encode(&d).unwrap();
        jobs.cancel_stale(&builder, scene.world(), 2, Tick(3), |_| true);
        let response = execute_gi_request(&request, || d.cancellation.is_cancelled()).unwrap();
        jobs.complete_bytes(d.id, &response).unwrap();
        assert_eq!(
            (
                jobs.stats.active,
                jobs.stats.ready,
                jobs.stats.retained_bytes
            ),
            (0, 0, 0)
        );
    }
}
