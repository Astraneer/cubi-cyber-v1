//! Incremental natural population owner: one immutable bounded worker and one
//! global private RNG continuation; completed populations share the mob WAL.
use super::*;
use jobs::{Finish, JobId, JobSpec, Priority, Scheduler, Take, WorkBudget, WorkCost};
use mob_spawn::{Kind, Status};
use natural::Column;
const MAX_MOBS: usize = 5000;
const RETAINED: usize = 12 * 1024 * 1024;
#[derive(Clone, Copy)]
struct Waiting {
    due: Tick,
    first: Tick,
    order: u64,
}
struct Context {
    column: Column,
    kind: Kind,
    players: [u8; 32],
    tick: Tick,
}
pub(super) struct Owner {
    workers: mob_spawn::Workers,
    scheduler: Scheduler<mob_spawn::Task>,
    pending: Option<(JobId, Context)>,
    loaded: BTreeMap<Column, u8>,
    active: BTreeSet<Column>,
    waiting: BTreeMap<Column, Waiting>,
    due: BTreeSet<(Tick, u64, Column)>,
    centers: Vec<(SpaceId, i32, i32)>,
    initialized: bool,
    order: u64,
    monster_cursor: Option<Column>,
    entity_seed: u64,
    attempt_tick: Tick,
    attempts: usize,
}
impl Owner {
    pub(super) fn new(world: &World, entity_seed: u64) -> Result<Self, ServerError> {
        let mut loaded = BTreeMap::new();
        for (page, _) in world.pages() {
            if page.space == SpaceId(0) && (0..8).contains(&page.y) {
                *loaded
                    .entry(Column {
                        space: page.space,
                        x: page.x,
                        z: page.z,
                    })
                    .or_insert(0) |= 1u8 << page.y;
            }
        }
        Ok(Self {
            workers: mob_spawn::Workers::new(1, 1)
                .map_err(|_| ServerError::Invalid("natural worker"))?,
            scheduler: Scheduler::new(jobs::Limits {
                max_jobs: 1,
                max_bytes: 16 * 1024 * 1024,
                max_age_ticks: 100,
                reserved_interactive_jobs: 0,
                reserved_interactive_bytes: 0,
            })
            .map_err(|_| ServerError::Invalid("natural job limits"))?,
            pending: None,
            loaded,
            active: BTreeSet::new(),
            waiting: BTreeMap::new(),
            due: BTreeSet::new(),
            centers: Vec::with_capacity(32),
            initialized: false,
            order: 0,
            monster_cursor: None,
            entity_seed,
            attempt_tick: Tick(0),
            attempts: 0,
        })
    }
    fn near(&self, column: Column) -> bool {
        self.centers.iter().any(|&(space, x, z)| {
            space == column.space
                && (i64::from(x) - i64::from(column.x)).abs() <= 8
                && (i64::from(z) - i64::from(column.z)).abs() <= 8
        })
    }
    fn register(&mut self, column: Column, now: Tick, state: &natural::State) {
        if self.loaded.get(&column) == Some(&255) && self.near(column) {
            self.active.insert(column);
            if !state.populated(column) && !self.waiting.contains_key(&column) {
                let waiting = Waiting {
                    due: now,
                    first: now,
                    order: self.order,
                };
                self.order = self.order.saturating_add(1);
                self.waiting.insert(column, waiting);
                self.due.insert((waiting.due, waiting.order, column));
            }
        } else {
            self.active.remove(&column);
            if let Some(old) = self.waiting.remove(&column) {
                self.due.remove(&(old.due, old.order, column));
            }
        }
    }
    fn retry(&mut self, column: Column, now: Tick) {
        if let Some(waiting) = self.waiting.get_mut(&column) {
            self.due.remove(&(waiting.due, waiting.order, column));
            waiting.due = Tick(now.0.saturating_add(20));
            self.due.insert((waiting.due, waiting.order, column));
        }
    }
    fn complete(&mut self, column: Column) {
        if let Some(waiting) = self.waiting.remove(&column) {
            self.due.remove(&(waiting.due, waiting.order, column));
        }
    }
    fn dispatch(&mut self, world: &World, now: Tick) {
        let mut budget = WorkBudget::unrestricted(WorkCost {
            cells: 8192,
            quads: 0,
            bytes: RETAINED as u64,
            rays: 64,
        });
        match self
            .scheduler
            .take_next(now, &mut budget, |page| current(world, page))
        {
            Take::Ready(dispatch) => {
                if let Err(error) = self.workers.try_submit(dispatch) {
                    match *error {
                        jobs::SubmitError::Full(dispatch) | jobs::SubmitError::Closed(dispatch) => {
                            let _ = self.scheduler.requeue(dispatch);
                        }
                    }
                }
            }
            Take::Expired(_) => {
                if let Some((_, context)) = self.pending.take() {
                    self.retry(context.column, now);
                }
            }
            Take::Empty | Take::BudgetBlocked => {}
        }
    }
}
fn current(world: &World, page: PageId) -> Option<Version> {
    world
        .page(page)
        .map(|_| world.input_version(page, mob_spawn::CHANNELS))
}
fn players_stamp(peers: &BTreeMap<u64, Peer>) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    for peer in peers
        .values()
        .filter(|p| p.ready && p.health.alive() && p.player.mode != GameMode::Spectator)
    {
        hash.update(&peer.identity.0);
        hash.update(&peer.space.0.to_le_bytes());
        for value in peer.player.pos {
            hash.update(&value.to_bits().to_le_bytes());
        }
    }
    *hash.finalize().as_bytes()
}
impl Server {
    pub(super) fn observe_natural(&mut self, changes: &[WorldChange]) {
        for page in changes
            .iter()
            .flat_map(|change| &change.pages)
            .filter(|p| p.created || p.removed)
        {
            if page.page.space != SpaceId(0) || !(0..8).contains(&page.page.y) {
                continue;
            }
            let column = Column {
                space: page.page.space,
                x: page.page.x,
                z: page.page.z,
            };
            let mask = self.natural_owner.loaded.entry(column).or_insert(0);
            if page.removed {
                *mask &= !(1u8 << page.page.y);
            } else {
                *mask |= 1u8 << page.page.y;
            }
            if *mask == 0 {
                self.natural_owner.loaded.remove(&column);
            }
            self.natural_owner
                .register(column, self.tick, &self.runtimes.natural);
        }
    }
    fn refresh_natural(&mut self) {
        let mut centers = [None; 32];
        let mut count = 0;
        for peer in self
            .peers
            .values()
            .filter(|p| p.ready && p.health.alive() && p.player.mode != GameMode::Spectator)
        {
            centers[count] = Some((
                peer.space,
                (peer.player.pos[0].floor() as i32).div_euclid(16),
                (peer.player.pos[2].floor() as i32).div_euclid(16),
            ));
            count += 1;
        }
        if self.natural_owner.initialized
            && self.natural_owner.centers.len() == count
            && self
                .natural_owner
                .centers
                .iter()
                .copied()
                .eq(centers[..count].iter().flatten().copied())
        {
            return;
        }
        self.natural_owner.initialized = true;
        self.natural_owner.centers.clear();
        self.natural_owner
            .centers
            .extend(centers[..count].iter().flatten().copied());
        self.natural_owner.active.clear();
        for &(space, x, z) in self
            .natural_owner
            .centers
            .clone()
            .iter()
            .filter(|c| c.0 == SpaceId(0))
        {
            for dz in -8..=8 {
                for dx in -8..=8 {
                    self.counters.natural_index_probes += 1;
                    self.natural_owner.register(
                        Column {
                            space,
                            x: x + dx,
                            z: z + dz,
                        },
                        self.tick,
                        &self.runtimes.natural,
                    );
                }
            }
        }
        let owner = &mut self.natural_owner;
        owner.waiting.retain(|column, waiting| {
            if owner.active.contains(column) {
                true
            } else {
                owner.due.remove(&(waiting.due, waiting.order, *column));
                false
            }
        });
    }
    pub(super) fn schedule_natural(&mut self) -> Result<(), ServerError> {
        self.refresh_natural();
        if self.natural_owner.attempt_tick != self.tick {
            self.natural_owner.attempt_tick = self.tick;
            self.natural_owner.attempts = 0;
        }
        if self.natural_owner.pending.is_some()
            || self.natural_owner.active.is_empty()
            || !self.height_ready()
            || self.natural_owner.attempts >= mob_spawn::HERD_COLUMNS_PER_TICK
        {
            return Ok(());
        }
        if self.counters.light_pending_cells != 0
            || self.counters.light_pending_seed_pages != 0
            || self.counters.light_pending_sky_pages != 0
        {
            return Ok(());
        }
        let herd = self
            .natural_owner
            .due
            .first()
            .filter(|(due, _, _)| *due <= self.tick)
            .map(|(_, _, column)| *column);
        let available = MAX_MOBS
            .saturating_sub(self.runtimes.mobs.entities().len() + self.mob_owner.pending_count());
        if available == 0 {
            return Ok(());
        }
        let (kind, column, available) = if let Some(column) = herd {
            (Kind::Herd, column, available)
        } else {
            let cap = mob_spawn::monster_cap(self.natural_owner.active.len());
            let room = cap
                .saturating_sub(self.runtimes.hostile_mobs)
                .min(available);
            if room == 0 {
                return Ok(());
            }
            let column = self
                .natural_owner
                .active
                .range((
                    std::ops::Bound::Excluded(self.natural_owner.monster_cursor.unwrap_or(
                        Column {
                            space: SpaceId(u32::MAX),
                            x: 0,
                            z: 0,
                        },
                    )),
                    std::ops::Bound::Unbounded,
                ))
                .next()
                .or_else(|| self.natural_owner.active.first())
                .copied()
                .expect("active columns");
            self.natural_owner.monster_cursor = Some(column);
            (Kind::Monster, column, room)
        };
        if let Some(waiting) = self.natural_owner.waiting.get(&column) {
            self.counters.natural_max_wait_age = self
                .counters
                .natural_max_wait_age
                .max(self.tick.0.saturating_sub(waiting.first.0));
        }
        let mut pages = Vec::with_capacity(200);
        for dz in -2..=2 {
            for dx in -2..=2 {
                let known = self.natural_owner.loaded.get(&Column {
                    x: column.x + dx,
                    z: column.z + dz,
                    ..column
                }) == Some(&255);
                for y in 0..8 {
                    pages.push((
                        PageId {
                            space: column.space,
                            x: column.x + dx,
                            y,
                            z: column.z + dz,
                        },
                        known,
                    ));
                }
            }
        }
        let snapshot = mob_spawn::Snapshot::capture(
            &self.world,
            r#gen::Generator::new(r#gen::TerrainSeed(self.config.public_seed)),
            column.space,
            &pages,
        )
        .and_then(|s| {
            s.with_spawn_protection(
                [self.spawn[0], self.spawn[2]],
                self.config.spawn_protection_radius,
            )
        })
        .map_err(|_| ServerError::Invalid("natural immutable snapshot"))?;
        let inputs = snapshot.captured_inputs();
        let request = mob_spawn::Request {
            kind,
            space: column.space,
            column: [column.x, column.z],
            tick: self.tick,
            map_seed: self.runtimes.natural.map_seed(),
            rng_before: self.runtimes.natural.rng(),
            available,
            day: self.tick.0 % 24000,
        };
        let players = self
            .peers
            .values()
            .filter(|p| p.ready && p.health.alive() && p.player.mode != GameMode::Spectator)
            .map(|p| mob_spawn::Player {
                space: p.space,
                position: p.player.pos,
            })
            .collect();
        let mut spec = JobSpec::derived(
            jobs::JobKey {
                page: PageId {
                    space: column.space,
                    x: column.x,
                    y: 0,
                    z: column.z,
                },
                kind: 91,
            },
            Priority::Background,
            WorkCost {
                cells: 8192,
                quads: 0,
                bytes: RETAINED as u64,
                rays: 64,
            },
            RETAINED,
            self.tick,
        );
        spec.input_versions = inputs;
        let job = self
            .natural_owner
            .scheduler
            .enqueue(
                spec,
                mob_spawn::Task {
                    request,
                    snapshot,
                    players,
                    budget: mob_spawn::Budget::default(),
                },
            )
            .map_err(|_| ServerError::Invalid("natural queue admission"))?;
        self.natural_owner.pending = Some((
            job,
            Context {
                column,
                kind,
                players: players_stamp(&self.peers),
                tick: self.tick,
            },
        ));
        self.natural_owner.attempts += 1;
        self.counters.natural_submitted += 1;
        self.natural_owner.dispatch(&self.world, self.tick);
        Ok(())
    }
    pub(super) fn finish_natural(&mut self) -> Result<bool, ServerError> {
        self.natural_owner.dispatch(&self.world, self.tick);
        let Some(completion) = self.natural_owner.workers.try_recv() else {
            self.schedule_natural()?;
            return Ok(false);
        };
        let Some((id, context)) = self.natural_owner.pending.take() else {
            return Err(ServerError::Invalid("orphan natural result"));
        };
        if id != completion.id {
            return Err(ServerError::Invalid("natural completion identity"));
        }
        let plan = match self
            .natural_owner
            .scheduler
            .finish(completion, |page| current(&self.world, page))
        {
            Finish::Published(plan) => plan,
            Finish::Stale | Finish::Cancelled | Finish::Failed(_) => {
                self.counters.natural_stale += 1;
                self.natural_owner.retry(context.column, self.tick);
                return Ok(false);
            }
            Finish::UnknownJob => return Err(ServerError::Invalid("unknown natural completion")),
        };
        self.counters.natural_worker_cells += plan.work.cell_reads;
        self.counters.natural_worker_columns += plan.work.column_queries;
        let available = MAX_MOBS
            .saturating_sub(self.runtimes.mobs.entities().len() + self.mob_owner.pending_count());
        let cap = if context.kind == Kind::Monster {
            mob_spawn::monster_cap(self.natural_owner.active.len())
                .saturating_sub(self.runtimes.hostile_mobs)
                .min(available)
        } else {
            available
        };
        let current_players = players_stamp(&self.peers);
        if plan.kind != context.kind
            || plan.column != [context.column.x, context.column.z]
            || plan.space != context.column.space
        {
            return Err(ServerError::Invalid("natural result context"));
        }
        if plan.status != Status::Complete
            || !self.natural_owner.active.contains(&context.column)
            || context.players != current_players
            || plan.rng_before != self.runtimes.natural.rng()
            || plan.candidates.len() > cap
            || !mob_spawn::Snapshot::validate_inputs(&self.world, &plan.inputs)
            || (context.kind == Kind::Monster
                && mob_spawn::ambient_darkness(context.tick.0 % 24000)
                    != mob_spawn::ambient_darkness(self.tick.0 % 24000))
        {
            self.counters.natural_deferred += 1;
            self.natural_owner.retry(context.column, self.tick);
            return Ok(false);
        }
        let mut next = self.runtimes.natural.next_id();
        let mut rows = Vec::with_capacity(plan.candidates.len());
        for candidate in plan.candidates {
            let id = loop {
                let id = rules::entities::EntityId((1u64 << 62) | next);
                next = next
                    .checked_add(1)
                    .ok_or(ServerError::Invalid("natural entity ids exhausted"))?;
                if self.runtimes.mobs.entities().get(id).is_none()
                    && self.runtimes.items.entities().get(id).is_none()
                    && !self.mob_owner.contains_pending(id)
                    && !self
                        .identities
                        .keys()
                        .any(|p| runtime::player_entity(*p) == id)
                {
                    break id;
                }
                if next - self.runtimes.natural.next_id() > 1024 {
                    self.natural_owner.retry(context.column, self.tick);
                    return Ok(false);
                }
            };
            let mob = candidate
                .instantiate(
                    id,
                    context.column.space,
                    self.natural_owner.entity_seed,
                    self.tick,
                )
                .map_err(|_| ServerError::Invalid("natural actor encoding"))?;
            rows.push((mob, self.mob_active_at(mob.space, mob.body.position)));
        }
        let metadata = match self.runtimes.natural.plan(
            plan.rng_before,
            plan.rng_after,
            next,
            (context.kind == Kind::Herd).then_some(context.column),
        ) {
            Ok(delta) => delta,
            Err(natural::Error::Capacity | natural::Error::Overflow | natural::Error::Bounds) => {
                self.counters.natural_deferred += 1;
                self.natural_owner.retry(context.column, self.tick);
                return Ok(false);
            }
            Err(_) => return Err(ServerError::Invalid("natural metadata admission")),
        };
        let mobs = if rows.is_empty() {
            None
        } else {
            Some(
                self.runtimes
                    .mobs
                    .extend_owned(
                        self.runtimes
                            .mobs
                            .plan_activity(&[])
                            .map_err(|_| ServerError::Invalid("natural actor revision"))?,
                        rows.iter().copied(),
                        &[],
                        &[],
                    )
                    .map_err(|_| ServerError::Invalid("natural actor admission"))?,
            )
        };
        let mut batch = Batch::empty(self.tick);
        if metadata.is_some() || mobs.is_some() {
            batch.runtimes.push(save::RuntimeRecord {
                kind: save::RUNTIME_MOBS,
                payload: natural_records::delta(mobs.as_ref(), metadata)
                    .map_err(ServerError::Invalid)?,
            });
        }
        if context.kind == Kind::Herd {
            self.natural_owner.complete(context.column);
        }
        self.counters.natural_completed += 1;
        self.counters.natural_births += rows.len() as u64;
        for (mob, active) in rows {
            if active {
                self.mob_owner.note_birth(mob.id);
            }
        }
        if batch.is_empty() {
            return Ok(false);
        }
        self.submit_batch(batch, Vec::new());
        Ok(true)
    }
}
