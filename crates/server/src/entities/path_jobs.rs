//! Real bounded native snapshot workers for entity paths. Only cell data crosses
//! the worker boundary; version checks happen again on the single owner.
use core_types::{BlockPos, Cell, ChannelMask, PAGE_VOLUME, PageId, Tick, Version};
use jobs::{
    Finish, JobId, JobSpec, Limits, NativePool, Priority, Scheduler, SubmitError, Take, WorkBudget,
    WorkCost,
};
use rules::entities::{EntityError, EntityId, mob::Mob};
use sim::{
    QueryWorld,
    entities::path::{NavPath, NavSearch, NavSpec, PathBudget, SearchStatus},
};
use std::{collections::BTreeMap, sync::Arc};
use world::{CellRead, World};

const CHANNELS: ChannelMask =
    ChannelMask(ChannelMask::COLLISION.0 | ChannelMask::FLUID.0 | ChannelMask::OPACITY.0);
pub struct PathResult {
    pub actor: EntityId,
    pub start: [i32; 3],
    pub goal: [i32; 3],
    pub path: Option<NavPath>,
    pub status: SearchStatus,
    pub inputs: Vec<jobs::InputVersion>,
    pub reads: u64,
    pub expansions: u64,
}
struct Task {
    actor: EntityId,
    start: [i32; 3],
    goal: [i32; 3],
    spec: NavSpec,
    snapshot: Snapshot,
    inputs: Vec<jobs::InputVersion>,
}
struct Snapshot {
    space: core_types::SpaceId,
    pages: Vec<(PageId, Option<Arc<[Cell; PAGE_VOLUME]>>)>,
}
impl QueryWorld for Snapshot {
    fn cell(&self, p: [i32; 3]) -> CellRead {
        let position = BlockPos {
            space: self.space,
            x: p[0],
            y: p[1],
            z: p[2],
        };
        self.pages
            .binary_search_by_key(&position.page(), |(page, _)| *page)
            .ok()
            .and_then(|index| self.pages[index].1.as_ref())
            .map_or(CellRead::Unknown, |values| {
                CellRead::Known(values[usize::from(position.local_index().get())])
            })
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub submitted: u64,
    pub completed: u64,
    pub stale: u64,
    pub failed: u64,
    pub expired: u64,
    pub snapshot_cells: u64,
    pub worker_reads: u64,
    pub expansions: u64,
    pub backpressure: u64,
    pub oldest_age: u64,
}
pub struct PathJobs {
    scheduler: Scheduler<Task>,
    pool: NativePool<Task, PathResult>,
    actors: BTreeMap<JobId, EntityId>,
    pending: BTreeMap<EntityId, JobId>,
    ready: BTreeMap<EntityId, PathResult>,
    maximum: usize,
    cache: BTreeMap<PageId, (Version, Arc<[Cell; PAGE_VOLUME]>)>,
    pub counters: Counters,
}
impl PathJobs {
    pub fn new(workers: usize, maximum: usize) -> Result<Self, EntityError> {
        if maximum == 0 || maximum > 64 {
            return Err(EntityError::Capacity);
        }
        let scheduler = Scheduler::new(Limits {
            max_jobs: maximum,
            max_bytes: 32 * 1024 * 1024,
            max_age_ticks: 100,
            reserved_interactive_jobs: 0,
            reserved_interactive_bytes: 0,
        })
        .map_err(|_| EntityError::Capacity)?;
        let pool = NativePool::new(workers, maximum, |task: Task, cancel| {
            let mut search = NavSearch::new(task.start, task.goal, task.spec, 0)
                .map_err(|error| format!("navigation {error:?}"))?;
            let mut reads = 0;
            let mut expansions = 0;
            loop {
                if cancel.is_cancelled() {
                    return Err("navigation cancelled".into());
                }
                let work = search.advance(&task.snapshot, PathBudget::default());
                reads += u64::from(work.reads);
                expansions += u64::from(work.expanded);
                if search.status() != SearchStatus::Pending {
                    break;
                }
            }
            Ok(PathResult {
                actor: task.actor,
                start: task.start,
                goal: task.goal,
                path: search.result().cloned(),
                status: search.status(),
                inputs: task.inputs,
                reads,
                expansions,
            })
        })
        .map_err(|_| EntityError::Capacity)?;
        Ok(Self {
            scheduler,
            pool,
            actors: BTreeMap::new(),
            pending: BTreeMap::new(),
            ready: BTreeMap::new(),
            maximum,
            cache: BTreeMap::new(),
            counters: Counters::default(),
        })
    }
    pub fn pending(&self, id: EntityId) -> bool {
        self.pending.contains_key(&id)
    }
    pub fn request(
        &mut self,
        world: &World,
        mob: &Mob,
        goal: [i32; 3],
        now: Tick,
        snapshot_cell_budget: &mut u64,
    ) -> Result<bool, EntityError> {
        if self.pending(mob.id) {
            return Ok(false);
        }
        if self.pending.len() + self.ready.len() >= self.maximum {
            self.counters.backpressure += 1;
            return Err(EntityError::Capacity);
        }
        let start = mob.body.position.map(|value| value.floor() as i32);
        let spec = NavSpec::for_mob(mob.kind, mob.baby);
        let low =
            std::array::from_fn::<_, 3, _>(|axis| start[axis].min(goal[axis]).div_euclid(16) - 1);
        let high =
            std::array::from_fn::<_, 3, _>(|axis| start[axis].max(goal[axis]).div_euclid(16) + 1);
        let count = (0..3)
            .try_fold(1usize, |count, axis| {
                count.checked_mul((i64::from(high[axis]) - i64::from(low[axis]) + 1) as usize)
            })
            .ok_or(EntityError::Capacity)?;
        if count > 125 {
            return Err(EntityError::Capacity);
        }
        let mut inputs = Vec::with_capacity(count);
        let mut known = 0u64;
        let mut copies = 0u64;
        for x in low[0]..=high[0] {
            for y in low[1]..=high[1] {
                for z in low[2]..=high[2] {
                    let page = PageId {
                        space: mob.space,
                        x,
                        y,
                        z,
                    };
                    let version = current(world, page);
                    known += u64::from(version.is_some());
                    if let Some(version) = version
                        && self
                            .cache
                            .get(&page)
                            .is_none_or(|(cached, _)| *cached != version)
                    {
                        copies += 1;
                    }
                    inputs.push(jobs::InputVersion { page, version });
                }
            }
        }
        let cells = copies * PAGE_VOLUME as u64;
        if cells > *snapshot_cell_budget {
            self.counters.backpressure += 1;
            return Err(EntityError::Capacity);
        }
        let retained = usize::try_from(known * PAGE_VOLUME as u64 * 4)
            .map_err(|_| EntityError::Capacity)?
            + spec.maximum_nodes * 512
            + count * std::mem::size_of::<jobs::InputVersion>() * 2
            + 4096;
        if self.scheduler.retained_bytes().saturating_add(retained) > 32 * 1024 * 1024 {
            self.counters.backpressure += 1;
            return Err(EntityError::Capacity);
        }
        let mut pages = Vec::with_capacity(count);
        *snapshot_cell_budget -= cells;
        self.counters.snapshot_cells += cells;
        let mut cache_updates = Vec::with_capacity(copies as usize);
        for input in &inputs {
            let values = if let Some(version) = input.version {
                if let Some((_, values)) = self
                    .cache
                    .get(&input.page)
                    .filter(|(cached, _)| *cached == version)
                {
                    Some(values.clone())
                } else {
                    let values: Arc<[Cell; PAGE_VOLUME]> = world
                        .page(input.page)
                        .ok_or(EntityError::UnknownInput)?
                        .cells()
                        .into();
                    cache_updates.push((input.page, version, values.clone()));
                    Some(values)
                }
            } else {
                self.cache.remove(&input.page);
                None
            };
            pages.push((input.page, values));
        }
        for (page, version, values) in cache_updates {
            if self.cache.len() >= 256 {
                self.cache.pop_first();
            }
            self.cache.insert(page, (version, values));
        }
        let mut job = JobSpec::event(
            Priority::Background,
            WorkCost {
                cells: spec.maximum_nodes as u64 * 512,
                quads: 0,
                bytes: retained as u64,
                rays: 0,
            },
            retained,
            now,
        );
        job.input_versions = inputs.clone();
        let id = self
            .scheduler
            .enqueue(
                job,
                Task {
                    actor: mob.id,
                    start,
                    goal,
                    spec,
                    snapshot: Snapshot {
                        space: mob.space,
                        pages,
                    },
                    inputs,
                },
            )
            .map_err(|_| {
                self.counters.backpressure += 1;
                EntityError::Capacity
            })?;
        self.counters.submitted += 1;
        self.actors.insert(id, mob.id);
        self.pending.insert(mob.id, id);
        Ok(true)
    }
    pub fn pump(&mut self, world: &World, now: Tick, budget: &mut WorkBudget) {
        for _ in 0..self.maximum {
            let Some(completion) = self.pool.try_recv() else {
                break;
            };
            let actor = self.actors.remove(&completion.id);
            if let Some(actor) = actor {
                self.pending.remove(&actor);
            }
            match self
                .scheduler
                .finish(completion, |page| current(world, page))
            {
                Finish::Published(result) => {
                    self.counters.worker_reads += result.reads;
                    self.counters.expansions += result.expansions;
                    self.counters.completed += 1;
                    if self.ready.len() < self.maximum {
                        self.ready.insert(result.actor, result);
                    } else {
                        self.counters.backpressure += 1;
                    }
                }
                Finish::Stale | Finish::Cancelled => self.counters.stale += 1,
                Finish::Failed(_) => self.counters.failed += 1,
                Finish::UnknownJob => {}
            }
        }
        for _ in 0..self.maximum {
            match self
                .scheduler
                .take_next(now, budget, |page| current(world, page))
            {
                Take::Ready(dispatch) => {
                    if let Err(SubmitError::Full(dispatch) | SubmitError::Closed(dispatch)) =
                        self.pool.try_submit(dispatch)
                    {
                        let _ = self.scheduler.requeue(dispatch);
                        break;
                    }
                }
                Take::Expired(expired) => {
                    self.counters.expired += 1;
                    self.counters.oldest_age = self.counters.oldest_age.max(expired.age_ticks);
                    self.actors.remove(&expired.id);
                    self.pending.remove(&expired.payload.actor);
                }
                Take::Empty | Take::BudgetBlocked => break,
            }
        }
    }
    pub fn take_ready(&mut self, id: EntityId) -> Option<PathResult> {
        self.ready.remove(&id)
    }
    pub fn cancel(&mut self, id: EntityId) {
        if let Some(job) = self.pending.remove(&id) {
            self.scheduler.cancel(job);
            self.actors.remove(&job);
        }
        self.ready.remove(&id);
    }
    pub fn inputs_match(world: &World, inputs: &[jobs::InputVersion]) -> bool {
        inputs
            .iter()
            .all(|input| current(world, input.page) == input.version)
    }
    pub fn stats(&self) -> jobs::JobStats {
        self.scheduler.stats()
    }
}
fn current(world: &World, page: PageId) -> Option<Version> {
    world
        .page(page)
        .map(|_| world.input_version(page, CHANNELS))
}
