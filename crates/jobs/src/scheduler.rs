use core_types::{PageId, Tick, Version};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Logical work, not elapsed time. A job must fit all four independent limits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkCost {
    pub cells: u64,
    pub quads: u64,
    pub bytes: u64,
    pub rays: u64,
}

impl WorkCost {
    pub const ZERO: Self = Self {
        cells: 0,
        quads: 0,
        bytes: 0,
        rays: 0,
    };

    pub const fn fits(self, available: Self) -> bool {
        self.cells <= available.cells
            && self.quads <= available.quads
            && self.bytes <= available.bytes
            && self.rays <= available.rays
    }

    fn subtract(self, cost: Self) -> Self {
        Self {
            cells: self.cells.saturating_sub(cost.cells),
            quads: self.quads.saturating_sub(cost.quads),
            bytes: self.bytes.saturating_sub(cost.bytes),
            rays: self.rays.saturating_sub(cost.rays),
        }
    }

    fn minimum(self, other: Self) -> Self {
        Self {
            cells: self.cells.min(other.cells),
            quads: self.quads.min(other.quads),
            bytes: self.bytes.min(other.bytes),
            rays: self.rays.min(other.rays),
        }
    }
}

/// Per-dispatch-phase allowance. Background work cannot consume the near-edit reserve.
#[derive(Clone, Copy, Debug)]
pub struct WorkBudget {
    remaining: WorkCost,
    interactive_reserve: WorkCost,
}

impl WorkBudget {
    pub fn new(total: WorkCost, interactive_reserve: WorkCost) -> Self {
        Self {
            remaining: total,
            interactive_reserve: interactive_reserve.minimum(total),
        }
    }

    pub fn unrestricted(total: WorkCost) -> Self {
        Self::new(total, WorkCost::ZERO)
    }
    pub const fn remaining(&self) -> WorkCost {
        self.remaining
    }

    /// The owner can release unused reservation at the end of its interactive phase.
    pub fn release_interactive_reserve(&mut self) {
        self.interactive_reserve = WorkCost::ZERO;
    }

    fn can_spend(&self, cost: WorkCost, priority: Priority) -> bool {
        let available = if priority == Priority::Interactive {
            self.remaining
        } else {
            self.remaining.subtract(self.interactive_reserve)
        };
        cost.fits(available)
    }

    fn spend(&mut self, cost: WorkCost, priority: Priority) {
        self.remaining = self.remaining.subtract(cost);
        if priority == Priority::Interactive {
            self.interactive_reserve = self
                .interactive_reserve
                .subtract(cost)
                .minimum(self.remaining);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub u64);

/// Kind is owned by the subsystem (terrain, light, LOD, generation, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobKey {
    pub page: PageId,
    pub kind: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobClass {
    Derived(JobKey),
    Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Interactive,
    Normal,
    Background,
}

/// `None` captures an unknown page, so generation cannot overwrite a newly loaded page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputVersion {
    pub page: PageId,
    pub version: Option<Version>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobSpec {
    pub class: JobClass,
    pub input_versions: Vec<InputVersion>,
    pub priority: Priority,
    pub cost: WorkCost,
    /// Worst-case retained snapshot/output bytes until `finish`, not an allocator measurement.
    pub retained_bytes: usize,
    pub submitted_at: Tick,
}

impl JobSpec {
    pub fn derived(
        key: JobKey,
        priority: Priority,
        cost: WorkCost,
        retained_bytes: usize,
        submitted_at: Tick,
    ) -> Self {
        Self {
            class: JobClass::Derived(key),
            input_versions: Vec::new(),
            priority,
            cost,
            retained_bytes,
            submitted_at,
        }
    }

    pub fn event(
        priority: Priority,
        cost: WorkCost,
        retained_bytes: usize,
        submitted_at: Tick,
    ) -> Self {
        Self {
            class: JobClass::Event,
            input_versions: Vec::new(),
            priority,
            cost,
            retained_bytes,
            submitted_at,
        }
    }

    pub fn with_input(mut self, page: PageId, version: Option<Version>) -> Self {
        self.input_versions.push(InputVersion { page, version });
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_jobs: usize,
    pub max_bytes: usize,
    pub max_age_ticks: u64,
    pub reserved_interactive_jobs: usize,
    pub reserved_interactive_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_jobs: 128,
            max_bytes: 8 * 1024 * 1024,
            max_age_ticks: 40,
            reserved_interactive_jobs: 16,
            reserved_interactive_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidLimits;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    JobLimit,
    ByteLimit,
    DuplicateInput,
    OlderInputs,
    IdExhausted,
}

#[derive(Debug)]
pub struct Rejected<T> {
    pub reason: RejectReason,
    pub spec: JobSpec,
    pub payload: T,
}

/// Cooperative cancellation is cheap to check inside a tile/page loop.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub struct Dispatch<T> {
    pub id: JobId,
    pub spec: JobSpec,
    pub payload: T,
    pub cancellation: CancellationToken,
}

#[derive(Debug)]
pub struct Expired<T> {
    pub id: JobId,
    pub spec: JobSpec,
    pub payload: T,
    pub age_ticks: u64,
}

#[derive(Debug)]
pub enum Take<T> {
    Ready(Dispatch<T>),
    Expired(Expired<T>),
    BudgetBlocked,
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobFailure {
    Error(String),
    Panic(String),
    WorkerDisconnected,
}

#[derive(Debug)]
pub enum JobOutcome<R> {
    Success(R),
    Cancelled,
    Failed(JobFailure),
}

#[derive(Debug)]
pub struct Completion<R> {
    pub id: JobId,
    pub outcome: JobOutcome<R>,
}

#[derive(Debug)]
pub enum Finish<R> {
    Published(R),
    Cancelled,
    Stale,
    Failed(JobFailure),
    UnknownJob,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobStats {
    pub enqueued: u64,
    pub coalesced: u64,
    pub dispatched: u64,
    pub expired: u64,
    pub stale: u64,
    pub cancelled: u64,
    pub failed: u64,
    pub published: u64,
    pub max_observed_age_ticks: u64,
}

struct Queued<T> {
    id: JobId,
    spec: JobSpec,
    payload: T,
}
struct Running {
    spec: JobSpec,
    cancellation: CancellationToken,
}

/// One deterministic owner schedules work and publishes completed snapshots.
pub struct Scheduler<T> {
    limits: Limits,
    queued: BTreeMap<JobId, Queued<T>>,
    running: BTreeMap<JobId, Running>,
    latest: BTreeMap<JobKey, JobId>,
    retained_bytes: usize,
    noninteractive_jobs: usize,
    noninteractive_bytes: usize,
    next_id: u64,
    stats: JobStats,
}

impl<T> Scheduler<T> {
    pub fn new(limits: Limits) -> Result<Self, InvalidLimits> {
        if limits.max_jobs == 0
            || limits.max_bytes == 0
            || limits.max_age_ticks == 0
            || limits.reserved_interactive_jobs > limits.max_jobs
            || limits.reserved_interactive_bytes > limits.max_bytes
        {
            return Err(InvalidLimits);
        }
        Ok(Self {
            limits,
            queued: BTreeMap::new(),
            running: BTreeMap::new(),
            latest: BTreeMap::new(),
            retained_bytes: 0,
            noninteractive_jobs: 0,
            noninteractive_bytes: 0,
            next_id: 1,
            stats: JobStats::default(),
        })
    }

    pub fn pending_len(&self) -> usize {
        self.queued.len()
    }
    pub fn running_len(&self) -> usize {
        self.running.len()
    }
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
    pub const fn stats(&self) -> JobStats {
        self.stats
    }

    pub fn oldest_pending_age(&self, now: Tick) -> u64 {
        self.queued
            .values()
            .map(|job| now.0.saturating_sub(job.spec.submitted_at.0))
            .max()
            .unwrap_or(0)
    }

    pub fn enqueue(&mut self, mut spec: JobSpec, payload: T) -> Result<JobId, Rejected<T>> {
        let mut inputs = BTreeSet::new();
        if spec
            .input_versions
            .iter()
            .any(|input| !inputs.insert(input.page))
        {
            return Err(Rejected {
                reason: RejectReason::DuplicateInput,
                spec,
                payload,
            });
        }
        let previous = match spec.class {
            JobClass::Derived(key) => self.latest.get(&key).copied(),
            JobClass::Event => None,
        };
        let old = previous.and_then(|id| {
            self.queued
                .get(&id)
                .map(|job| &job.spec)
                .or_else(|| self.running.get(&id).map(|job| &job.spec))
        });
        if old.is_some_and(|old| {
            spec.input_versions.iter().any(|new| {
                old.input_versions.iter().any(|old| {
                    old.page == new.page
                        && matches!((new.version, old.version), (Some(new), Some(old)) if new < old)
                })
            })
        }) {
            return Err(Rejected {
                reason: RejectReason::OlderInputs,
                spec,
                payload,
            });
        }
        let replace = previous.and_then(|id| self.queued.get(&id));
        let old_bytes = replace.map_or(0, |old| old.spec.retained_bytes);
        let old_noninteractive =
            replace.is_some_and(|old| old.spec.priority != Priority::Interactive);
        let jobs = self.queued.len() + self.running.len() - usize::from(replace.is_some());
        let bytes = self.retained_bytes - old_bytes;
        let normal_jobs = self.noninteractive_jobs - usize::from(old_noninteractive);
        let normal_bytes =
            self.noninteractive_bytes - if old_noninteractive { old_bytes } else { 0 };
        let normal = spec.priority != Priority::Interactive;
        if jobs >= self.limits.max_jobs
            || (normal
                && normal_jobs >= self.limits.max_jobs - self.limits.reserved_interactive_jobs)
        {
            return Err(Rejected {
                reason: RejectReason::JobLimit,
                spec,
                payload,
            });
        }
        if spec.retained_bytes > self.limits.max_bytes.saturating_sub(bytes)
            || (normal
                && spec.retained_bytes
                    > (self.limits.max_bytes - self.limits.reserved_interactive_bytes)
                        .saturating_sub(normal_bytes))
        {
            return Err(Rejected {
                reason: RejectReason::ByteLimit,
                spec,
                payload,
            });
        }
        let Some(next_id) = self.next_id.checked_add(1) else {
            return Err(Rejected {
                reason: RejectReason::IdExhausted,
                spec,
                payload,
            });
        };
        let id = JobId(self.next_id);
        self.next_id = next_id;
        if let Some(old_id) = previous {
            if let Some(old) = self.queued.remove(&old_id) {
                // Repeated edits cannot postpone a page forever by resetting its age.
                spec.submitted_at = Tick(spec.submitted_at.0.min(old.spec.submitted_at.0));
                self.release(&old.spec);
                self.stats.coalesced = self.stats.coalesced.saturating_add(1);
            } else if let Some(old) = self.running.get(&old_id) {
                old.cancellation.cancel();
            }
        }
        self.retain(&spec);
        if let JobClass::Derived(key) = spec.class {
            self.latest.insert(key, id);
        }
        self.queued.insert(id, Queued { id, spec, payload });
        self.stats.enqueued = self.stats.enqueued.saturating_add(1);
        Ok(id)
    }

    /// Expiry is explicit: even a gameplay event comes back with its intact payload.
    /// Invalid derived snapshots can be discarded; callers retain authoritative events.
    pub fn take_next(
        &mut self,
        now: Tick,
        budget: &mut WorkBudget,
        current_version: impl Fn(PageId) -> Option<Version>,
    ) -> Take<T> {
        loop {
            let expired = self
                .queued
                .values()
                .find(|job| {
                    now.0.saturating_sub(job.spec.submitted_at.0) >= self.limits.max_age_ticks
                })
                .map(|job| job.id);
            if let Some(id) = expired {
                let job = self.remove_queued(id).expect("selected queued job");
                let age = now.0.saturating_sub(job.spec.submitted_at.0);
                self.stats.expired = self.stats.expired.saturating_add(1);
                self.stats.max_observed_age_ticks = self.stats.max_observed_age_ticks.max(age);
                return Take::Expired(Expired {
                    id,
                    spec: job.spec,
                    payload: job.payload,
                    age_ticks: age,
                });
            }
            let stale = self
                .queued
                .values()
                .find(|job| {
                    matches!(job.spec.class, JobClass::Derived(_))
                        && !inputs_match(&job.spec, &current_version)
                })
                .map(|job| job.id);
            if let Some(id) = stale {
                self.remove_queued(id);
                self.stats.stale = self.stats.stale.saturating_add(1);
                continue;
            }
            break;
        }
        let selection = self
            .queued
            .values()
            .filter(|job| budget.can_spend(job.spec.cost, job.spec.priority))
            .min_by_key(|job| {
                let age = now.0.saturating_sub(job.spec.submitted_at.0);
                let promoted = age >= self.limits.max_age_ticks.div_ceil(2);
                (
                    if promoted {
                        Priority::Interactive
                    } else {
                        job.spec.priority
                    },
                    job.spec.submitted_at,
                    job.id,
                )
            })
            .map(|job| job.id);
        let Some(id) = selection else {
            return if self.queued.is_empty() {
                Take::Empty
            } else {
                Take::BudgetBlocked
            };
        };
        let job = self.queued.remove(&id).expect("selected queued job");
        budget.spend(job.spec.cost, job.spec.priority);
        self.stats.max_observed_age_ticks = self
            .stats
            .max_observed_age_ticks
            .max(now.0.saturating_sub(job.spec.submitted_at.0));
        self.stats.dispatched = self.stats.dispatched.saturating_add(1);
        let cancellation = CancellationToken::new();
        self.running.insert(
            id,
            Running {
                spec: job.spec.clone(),
                cancellation: cancellation.clone(),
            },
        );
        Take::Ready(Dispatch {
            id,
            spec: job.spec,
            payload: job.payload,
            cancellation,
        })
    }

    /// Return a dispatch rejected by a full executor to the same bounded queue.
    /// Its identity and original deadline survive; no gameplay action is duplicated.
    pub fn requeue(&mut self, dispatch: Dispatch<T>) -> Result<(), Dispatch<T>> {
        let Some(running) = self.running.get(&dispatch.id) else {
            return Err(dispatch);
        };
        if running.spec != dispatch.spec || running.cancellation.is_cancelled() {
            return Err(dispatch);
        }
        self.running.remove(&dispatch.id);
        self.queued.insert(
            dispatch.id,
            Queued {
                id: dispatch.id,
                spec: dispatch.spec,
                payload: dispatch.payload,
            },
        );
        Ok(())
    }

    pub fn cancel(&mut self, id: JobId) -> Option<T> {
        if let Some(job) = self.remove_queued(id) {
            self.stats.cancelled = self.stats.cancelled.saturating_add(1);
            return Some(job.payload);
        }
        if let Some(job) = self.running.get(&id) {
            job.cancellation.cancel();
        }
        None
    }

    /// Only the owner calls this, using the current authoritative input versions.
    pub fn finish<R>(
        &mut self,
        completion: Completion<R>,
        current_version: impl Fn(PageId) -> Option<Version>,
    ) -> Finish<R> {
        let Some(running) = self.running.remove(&completion.id) else {
            return Finish::UnknownJob;
        };
        self.release(&running.spec);
        let latest = match running.spec.class {
            JobClass::Derived(key) => {
                let matches = self.latest.get(&key) == Some(&completion.id);
                if matches {
                    self.latest.remove(&key);
                }
                matches
            }
            JobClass::Event => true,
        };
        if running.cancellation.is_cancelled()
            || matches!(completion.outcome, JobOutcome::Cancelled)
        {
            self.stats.cancelled = self.stats.cancelled.saturating_add(1);
            return Finish::Cancelled;
        }
        if !latest || !inputs_match(&running.spec, &current_version) {
            self.stats.stale = self.stats.stale.saturating_add(1);
            return Finish::Stale;
        }
        match completion.outcome {
            JobOutcome::Success(result) => {
                self.stats.published = self.stats.published.saturating_add(1);
                Finish::Published(result)
            }
            JobOutcome::Failed(error) => {
                self.stats.failed = self.stats.failed.saturating_add(1);
                Finish::Failed(error)
            }
            JobOutcome::Cancelled => unreachable!("handled above"),
        }
    }

    fn remove_queued(&mut self, id: JobId) -> Option<Queued<T>> {
        let job = self.queued.remove(&id)?;
        self.release(&job.spec);
        if let JobClass::Derived(key) = job.spec.class
            && self.latest.get(&key) == Some(&id)
        {
            self.latest.remove(&key);
        }
        Some(job)
    }

    fn retain(&mut self, spec: &JobSpec) {
        self.retained_bytes += spec.retained_bytes;
        if spec.priority != Priority::Interactive {
            self.noninteractive_jobs += 1;
            self.noninteractive_bytes += spec.retained_bytes;
        }
    }

    fn release(&mut self, spec: &JobSpec) {
        self.retained_bytes -= spec.retained_bytes;
        if spec.priority != Priority::Interactive {
            self.noninteractive_jobs -= 1;
            self.noninteractive_bytes -= spec.retained_bytes;
        }
    }
}

fn inputs_match(spec: &JobSpec, current_version: &impl Fn(PageId) -> Option<Version>) -> bool {
    spec.input_versions
        .iter()
        .all(|input| current_version(input.page) == input.version)
}
