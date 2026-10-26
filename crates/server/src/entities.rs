//! Single-owner mob phase with actual snapshot path workers and detached durable
//! ledger updates. The caller commits world edits/drops/player hits with the same
//! WAL batch, acknowledges this delta, then sends public poses.
pub mod interactions;
pub mod items;
pub mod owner;
pub mod path_jobs;
pub mod replication;
use core_types::{BlockPos, Cell, ChannelMask, PageId, SpaceId, Tick, Version};
use jobs::{WorkBudget, WorkCost};
use path_jobs::{PathJobs, PathResult};
use rules::{
    blocks::bdef,
    entities::{
        EntityError, EntityId, MobKind,
        mob::{
            self, AiBudget, Control, EntityPatch, EntitySnapshot, EntityStore, Environment, Mob,
            PlayerIndex, PlayerProbe,
        },
    },
    ids::*,
    stack::ItemStack,
    survival::health::DamageKind,
};
use sim::{
    QueryWorld,
    collision::Bufs,
    entities::{self as movement, path::NavPath},
};
use std::{
    cell::{Cell as Counter, RefCell},
    collections::{BTreeMap, BTreeSet},
};
use world::{CellRead, Transaction, World};

const CHANNELS: ChannelMask = ChannelMask(
    ChannelMask::COLLISION.0
        | ChannelMask::FLUID.0
        | ChannelMask::OPACITY.0
        | ChannelMask::LIGHTING.0,
);
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub maximum_entities: usize,
    pub seed: u64,
    pub path_workers: usize,
    pub maximum_path_jobs: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            maximum_entities: 5000,
            seed: 0,
            path_workers: 2,
            maximum_path_jobs: 16,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub actors: usize,
    pub cells: u64,
    pub ai: AiBudget,
    pub snapshot_cells: u64,
    pub path_dispatch: WorkCost,
    pub sky_darkness: u8,
    pub push_candidates: u64,
    pub push_probes: u64,
    pub push_buckets: u64,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            actors: 1024,
            cells: 2_048_000,
            ai: AiBudget {
                buckets: 1_000_000,
                probes: 4_000_000,
                candidates: 100_000,
            },
            snapshot_cells: 131_072,
            path_dispatch: WorkCost {
                cells: 2_000_000,
                quads: 0,
                bytes: 32 * 1024 * 1024,
                rays: 0,
            },
            sky_darkness: 0,
            push_candidates: 16_384,
            push_probes: 131_072,
            push_buckets: 16_384,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub visited: u64,
    pub actors: u64,
    pub queries: u64,
    pub waiting_world: u64,
    pub waiting_path: u64,
    pub deferred_actors: u64,
    pub oldest_age: u64,
    pub budget_exhausted: bool,
    pub push_candidates: u64,
    pub push_probes: u64,
    pub push_buckets: u64,
    pub push_pairs: u64,
    pub push_deferred: u64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MobAttack {
    pub attacker: EntityId,
    pub target: EntityId,
    pub amount: f32,
    pub from: [f64; 3],
    pub space: SpaceId,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DeathDrops {
    pub actor: EntityId,
    pub space: SpaceId,
    pub position: [f64; 3],
    pub action: u64,
    pub stacks: [ItemStack; 3],
}
#[derive(Clone, Debug)]
pub struct MobDelta {
    revision: u64,
    next_revision: u64,
    cursor_before: usize,
    cursor_after: usize,
    pub patches: Vec<EntityPatch>,
}
pub struct PhasePlan {
    pub delta: MobDelta,
    pub transactions: Vec<Transaction>,
    pub attacks: Vec<MobAttack>,
    pub drops: Vec<DeathDrops>,
    pub inputs: Vec<jobs::InputVersion>,
    pub work: Work,
}
struct Route {
    path: NavPath,
    index: usize,
    goal: [i32; 3],
    inputs: Vec<jobs::InputVersion>,
}
pub struct MobRuntime {
    store: EntityStore,
    seed: u64,
    revision: u64,
    paths: PathJobs,
    routes: BTreeMap<EntityId, Route>,
    players: PlayerIndex,
    bufs: Bufs,
    maximum: usize,
    scratch: rules::entities::spatial::SpatialHash,
    scratch_ids: Vec<EntityId>,
}
impl MobRuntime {
    pub fn new(config: Config) -> Result<Self, EntityError> {
        Ok(Self {
            store: EntityStore::new(config.maximum_entities)?,
            seed: config.seed,
            revision: 0,
            paths: PathJobs::new(config.path_workers, config.maximum_path_jobs)?,
            routes: BTreeMap::new(),
            players: PlayerIndex::new(64)?,
            bufs: Bufs::default(),
            maximum: config.maximum_entities,
            scratch: rules::entities::spatial::SpatialHash::new(config.maximum_entities, 8.0)?,
            scratch_ids: Vec::with_capacity(config.maximum_entities),
        })
    }
    pub fn entities(&self) -> &EntityStore {
        &self.store
    }
    pub fn path_counters(&self) -> path_jobs::Counters {
        self.paths.counters
    }
    /// Startup fixture/recovery entry. Accepted live spawns use plan_spawn.
    pub fn insert_initial(&mut self, mob: Mob, active: bool) -> Result<(), EntityError> {
        self.store.insert(mob, active)
    }
    pub fn plan_spawn(&self, mob: Mob, active: bool) -> Result<MobDelta, EntityError> {
        let patches = vec![EntityPatch {
            before: None,
            after: Some(mob),
            before_active: false,
            after_active: active,
        }];
        self.store.validate_patches(&patches)?;
        self.delta(patches, self.store.active_cursor())
    }
    /// AOI changes are explicit events, never a cold world scan in the tick.
    pub fn plan_activity(&self, changes: &[(EntityId, bool)]) -> Result<MobDelta, EntityError> {
        if changes.len() > 1024 {
            return Err(EntityError::Capacity);
        }
        let patches = changes
            .iter()
            .map(|&(id, active)| {
                let mob = *self.store.get(id).ok_or(EntityError::UnknownId)?;
                Ok(EntityPatch {
                    before: Some(mob),
                    after: Some(mob),
                    before_active: self.store.is_active(id),
                    after_active: active,
                })
            })
            .collect::<Result<Vec<_>, EntityError>>()?;
        self.store.validate_patches(&patches)?;
        self.delta(patches, self.store.active_cursor())
    }
    fn delta(
        &self,
        patches: Vec<EntityPatch>,
        cursor_after: usize,
    ) -> Result<MobDelta, EntityError> {
        Ok(MobDelta {
            revision: self.revision,
            next_revision: self.revision.checked_add(1).ok_or(EntityError::Capacity)?,
            cursor_before: self.store.active_cursor(),
            cursor_after,
            patches,
        })
    }
    pub fn validate(&self, world: &World, plan: &PhasePlan) -> Result<(), EntityError> {
        if plan.delta.revision != self.revision
            || plan.delta.cursor_before != self.store.active_cursor()
            || !plan.inputs.iter().all(|input| {
                world
                    .page(input.page)
                    .map(|_| world.input_version(input.page, CHANNELS))
                    == input.version
            })
        {
            return Err(EntityError::StalePlan);
        }
        self.store.validate_patches(&plan.delta.patches)
    }
    /// Call only after all ledger/world/drop/player changes have reached fsync.
    pub fn acknowledge(&mut self, delta: MobDelta) -> Result<(), EntityError> {
        if delta.revision != self.revision
            || delta.next_revision != self.revision.checked_add(1).ok_or(EntityError::Capacity)?
            || delta.cursor_before != self.store.active_cursor()
        {
            return Err(EntityError::StalePlan);
        }
        self.store.apply_patches(&delta.patches)?;
        let cursor = if self.store.active_len() == 0 {
            0
        } else {
            delta.cursor_after % self.store.active_len()
        };
        self.store
            .publish_active_cursor(self.store.active_cursor(), cursor)?;
        self.revision = delta.next_revision;
        Ok(())
    }
    pub fn plan_tick(
        &mut self,
        world: &World,
        now: Tick,
        players: &[PlayerProbe],
        mut budget: Budget,
    ) -> Result<PhasePlan, EntityError> {
        self.players.rebuild(players)?;
        let mut dispatch = WorkBudget::unrestricted(budget.path_dispatch);
        self.paths.pump(world, now, &mut dispatch);
        let mut selected = vec![EntityId(0); budget.actors.min(self.store.active_len())];
        let (count, _) = self.store.peek_active_batch(budget.actors, &mut selected);
        selected.truncate(count);
        let reads = Counter::new(0u64);
        let exhausted = Counter::new(false);
        let inputs = RefCell::new(BTreeMap::new());
        let mut patches = Vec::with_capacity(count);
        let mut attacks = Vec::new();
        let mut drops = Vec::new();
        let mut edits = BTreeMap::<BlockPos, Cell>::new();
        let mut work = Work {
            deferred_actors: self.store.active_len().saturating_sub(count) as u64,
            ..Work::default()
        };
        let mut admitted = 0usize;
        for id in selected {
            admitted += 1;
            let before = *self.store.get(id).ok_or(EntityError::UnknownId)?;
            if now <= before.last_tick {
                continue;
            }
            work.oldest_age = work
                .oldest_age
                .max(now.0.saturating_sub(before.last_tick.0));
            let query = Tracked {
                world,
                space: before.space,
                reads: &reads,
                maximum: budget.cells,
                exhausted: &exhausted,
                inputs: &inputs,
            };
            let result = self.plan_actor(before, &query, now, &mut budget, &mut work);
            if exhausted.get() {
                admitted -= 1;
                work.budget_exhausted = true;
                work.deferred_actors += count.saturating_sub(admitted) as u64;
                break;
            }
            match result {
                Ok((after, control)) => {
                    // Grass consumption performs two final guarded reads after
                    // the body/AI plan. Retain this actor for the next owner
                    // microbatch rather than turn quota pressure into Unknown.
                    if control.eat_grass && reads.get().saturating_add(2) > budget.cells {
                        admitted -= 1;
                        work.budget_exhausted = true;
                        work.deferred_actors += count.saturating_sub(admitted) as u64;
                        break;
                    }
                    work.actors += 1;
                    if control.lay_egg {
                        drops.push(DeathDrops {
                            actor: id,
                            space: after.space,
                            position: after.body.position,
                            action: after.age | 1u64 << 63,
                            stacks: [
                                ItemStack::new(rules::items::I_EGG, 1)
                                    .map_err(|_| EntityError::InvalidEncoding)?,
                                ItemStack::EMPTY,
                                ItemStack::EMPTY,
                            ],
                        });
                    }
                    if let Some(target) = control.attack {
                        attacks.push(MobAttack {
                            attacker: id,
                            target,
                            amount: after.kind.definition().attack,
                            from: after.body.position,
                            space: after.space,
                        });
                    }
                    if control.eat_grass {
                        let feet = after.body.position.map(|value| value.floor() as i32);
                        let at = if matches!(query.cell(feet),CellRead::Known(cell) if cell.id()==TALL_GRASS)
                        {
                            feet
                        } else {
                            [feet[0], feet[1] - 1, feet[2]]
                        };
                        let CellRead::Known(cell) = query.cell(at) else {
                            return Err(EntityError::UnknownInput);
                        };
                        if cell.id() == TALL_GRASS || bdef(cell.id()).grass {
                            edits.insert(
                                BlockPos {
                                    space: after.space,
                                    x: at[0],
                                    y: at[1],
                                    z: at[2],
                                },
                                Cell::from_parts(
                                    if cell.id() == TALL_GRASS { AIR } else { DIRT },
                                    0,
                                ),
                            );
                        }
                    }
                    let mut after = after;
                    if !after.life.alive() && !after.loot_emitted {
                        let mut rng = core_types::DeterministicRng::from_key(
                            self.seed,
                            0x4d4f424c4f4f54,
                            id.0,
                            after.age,
                        );
                        drops.push(DeathDrops {
                            actor: id,
                            space: after.space,
                            position: after.body.position,
                            action: after.age,
                            stacks: mob::mob_loot(
                                &after,
                                after
                                    .last_player_hit
                                    .is_some_and(|(_, at)| now.0.saturating_sub(at.0) <= 100),
                                &mut rng,
                            ),
                        });
                        after.loot_emitted = true;
                    }
                    let remove = !after.life.alive() && after.life.death_time() >= 20;
                    if remove {
                        self.paths.cancel(id);
                        self.routes.remove(&id);
                    }
                    patches.push(EntityPatch {
                        before: Some(before),
                        after: (!remove).then_some(after),
                        before_active: true,
                        after_active: !remove,
                    });
                }
                Err(EntityError::UnknownInput) => {
                    work.waiting_world += 1;
                }
                Err(EntityError::Capacity) => {
                    admitted -= 1;
                    work.budget_exhausted = true;
                    work.deferred_actors += count.saturating_sub(admitted) as u64;
                    break;
                }
                Err(error) => return Err(error),
            }
            if exhausted.get() {
                work.budget_exhausted = true;
                break;
            }
        }
        work.visited = admitted as u64;
        work.queries = reads.get();
        let cursor = if self.store.active_len() == 0 {
            0
        } else {
            (self.store.active_cursor() + admitted) % self.store.active_len()
        };
        let mut inputs = inputs.into_inner();
        let mut transactions = BTreeMap::<SpaceId, Transaction>::new();
        for (position, value) in edits {
            transactions
                .entry(position.space)
                .or_insert_with(|| Transaction::new(now, position.space))
                .expect_page(position.page(), world.page_version(position.page()))
                .set_cell(position, value);
        }
        let patches = self.push_apart(
            world,
            patches,
            rules::entities::spatial::QueryBudget {
                candidates: budget.push_candidates,
                probes: budget.push_probes,
                buckets: budget.push_buckets,
            },
            &mut inputs,
            &mut work,
        )?;
        let inputs = inputs
            .into_iter()
            .map(|(page, version)| jobs::InputVersion { page, version })
            .collect();
        Ok(PhasePlan {
            delta: self.delta(patches, cursor)?,
            transactions: transactions.into_values().collect(),
            attacks,
            drops,
            inputs,
            work,
        })
    }
    fn push_apart(
        &mut self,
        world: &World,
        patches: Vec<EntityPatch>,
        mut remaining: rules::entities::spatial::QueryBudget,
        inputs: &mut BTreeMap<PageId, Option<Version>>,
        work: &mut Work,
    ) -> Result<Vec<EntityPatch>, EntityError> {
        if patches.is_empty() {
            return Ok(patches);
        }
        for index in 0..self.scratch_ids.len() {
            self.scratch.remove(index)?;
        }
        self.scratch_ids.clear();
        let mut staged: BTreeMap<_, _> = patches
            .into_iter()
            .map(|patch| Ok((patch.id()?, patch)))
            .collect::<Result<_, EntityError>>()?;
        for (&id, patch) in &staged {
            if let Some(after) = patch.after.filter(|mob| mob.life.alive()) {
                let index = self.scratch_ids.len();
                self.scratch.update(index, after.body.position)?;
                self.scratch_ids.push(id);
            }
        }
        let mut pairs = BTreeSet::new();
        let mut deltas = BTreeMap::<EntityId, [f64; 2]>::new();
        for id in self.scratch_ids.iter().copied() {
            if remaining.candidates == 0 || remaining.probes == 0 || remaining.buckets == 0 {
                work.push_deferred += 1;
                break;
            }
            let Some(mob) = current_mob(&self.store, &staged, id) else {
                continue;
            };
            let bounds = mob.bbox();
            let low = [bounds.lo[0] - 0.5, bounds.lo[1] - 2.0, bounds.lo[2] - 0.5];
            let high = [bounds.hi[0] + 0.5, bounds.hi[1], bounds.hi[2] + 0.5];
            let mut ids = [EntityId(0); 256];
            let (len, source) = self.store.query_ids(
                low,
                high,
                rules::entities::spatial::QueryBudget {
                    buckets: remaining.buckets.min(128),
                    probes: remaining.probes.min(1024),
                    candidates: remaining.candidates.min(256),
                },
                &mut ids,
            )?;
            charge_push(source, &mut remaining, work);
            let mut candidates: BTreeSet<_> = ids[..len].iter().copied().collect();
            let scratch = self.scratch.query(
                low,
                high,
                rules::entities::spatial::QueryBudget {
                    buckets: remaining.buckets.min(128),
                    probes: remaining.probes.min(1024),
                    candidates: remaining.candidates.min(256),
                },
                |index| {
                    candidates.insert(self.scratch_ids[index]);
                },
            )?;
            charge_push(scratch, &mut remaining, work);
            for other in candidates {
                if other == id || !self.store.is_active(other) {
                    continue;
                }
                let pair = if id < other { (id, other) } else { (other, id) };
                if !pairs.insert(pair) {
                    continue;
                }
                let Some(target) = current_mob(&self.store, &staged, other) else {
                    continue;
                };
                if !target.life.alive()
                    || target.space != mob.space
                    || !target.bbox().intersects(&bounds)
                {
                    continue;
                }
                if !staged.contains_key(&other)
                    && !known_push_body(world, target, &mut remaining, inputs, work)
                {
                    work.push_deferred += 1;
                    continue;
                }
                if let Some([x, z]) =
                    rules::entities::combat::separation(mob.body.position, target.body.position)
                {
                    let a = deltas.entry(id).or_default();
                    a[0] -= 2.0 * x;
                    a[1] -= 2.0 * z;
                    let b = deltas.entry(other).or_default();
                    b[0] += 2.0 * x;
                    b[1] += 2.0 * z;
                    work.push_pairs += 1;
                }
            }
            let players = self.players.overlapping(
                mob.space,
                bounds,
                rules::entities::spatial::QueryBudget {
                    buckets: remaining.buckets.min(128),
                    probes: remaining.probes.min(1024),
                    candidates: remaining.candidates.min(64),
                },
                |player| {
                    if let Some([x, z]) =
                        rules::entities::combat::separation(player.position, mob.body.position)
                    {
                        let velocity = deltas.entry(id).or_default();
                        velocity[0] += 2.0 * x;
                        velocity[1] += 2.0 * z;
                    }
                },
            )?;
            charge_push(players, &mut remaining, work);
        }
        for (id, delta) in deltas {
            let Some(mut after) = current_mob(&self.store, &staged, id) else {
                continue;
            };
            after.body.velocity[0] = (after.body.velocity[0] + delta[0]).clamp(-64.0, 64.0);
            after.body.velocity[2] = (after.body.velocity[2] + delta[1]).clamp(-64.0, 64.0);
            let patch = staged.entry(id).or_insert_with(|| EntityPatch {
                before: self.store.get(id).copied(),
                after: Some(after),
                before_active: self.store.is_active(id),
                after_active: self.store.is_active(id),
            });
            patch.after = Some(after);
        }
        Ok(staged.into_values().collect())
    }
    fn plan_actor(
        &mut self,
        before: Mob,
        world: &Tracked<'_>,
        now: Tick,
        budget: &mut Budget,
        work: &mut Work,
    ) -> Result<(Mob, Control), EntityError> {
        let mut after = before;
        let environment = movement::sample_environment(&after, world)?;
        after.begin_tick();
        after.last_tick = now;
        after.body.in_water = environment.water;
        after.body.in_lava = environment.lava;
        if after.life.alive() {
            if let Some(damage) = after.life.tick_fire(environment.water, environment.lava) {
                after.life.damage(
                    DamageKind::OnFire,
                    damage,
                    (after.kind.definition().armor, 0.0),
                    false,
                );
            }
            if environment.lava {
                after.life.ignite(300);
                after.life.damage(
                    DamageKind::Lava,
                    4.0,
                    (after.kind.definition().armor, 0.0),
                    false,
                );
            }
            if after.kind != MobKind::Zombie
                && let Some(damage) = after.life.tick_air(environment.head_under_water)
            {
                after.life.damage(
                    DamageKind::Drown,
                    damage,
                    (after.kind.definition().armor, 0.0),
                    false,
                );
            }
        }
        after.life.base_tick_end();
        let lay_egg = if after.kind == MobKind::Chicken && !after.baby && after.life.alive() {
            after.egg_remaining = after.egg_remaining.saturating_sub(1);
            if after.egg_remaining == 0 {
                let mut rng = core_types::DeterministicRng::from_key(
                    self.seed,
                    0x434849434b454747,
                    after.id.0,
                    after.age,
                );
                after.egg_remaining = 6000 + rng.below(6000).expect("egg interval") as u16;
                true
            } else {
                false
            }
        } else {
            false
        };
        let feet = after.body.position.map(|value| value.floor() as i32);
        let CellRead::Known(ground) = world.cell([feet[0], feet[1] - 1, feet[2]]) else {
            return Err(EntityError::UnknownInput);
        };
        let eye = BlockPos {
            space: after.space,
            x: feet[0],
            y: (after.body.position[1] + after.kind.definition().eye).floor() as i32,
            z: feet[2],
        };
        let light = world
            .world
            .page(eye.page())
            .map(|page| page.light(eye.local_index()))
            .unwrap_or_default();
        let (mut after, mut control) = after.plan_ai(
            now,
            self.seed,
            &self.players,
            Environment {
                grass: bdef(ground.id()).grass,
                sky_visible: light.sky == 15,
                sky_light: light.sky,
                darkness: budget.sky_darkness.min(15),
            },
            &mut budget.ai,
        )?;
        control.lay_egg = lay_egg;
        if after.kind == MobKind::Zombie {
            if let Some(target) = after
                .ai
                .target
                .and_then(|id| self.players_snapshot_target(id))
            {
                let eye = [
                    after.body.position[0],
                    after.body.position[1] + after.kind.definition().eye,
                    after.body.position[2],
                ];
                if movement::line_of_sight(world, eye, target.eye)? {
                    after.ai.goal_until =
                        Tick(
                            now.0
                                .saturating_add(if after.ai.revenge_mode { 300 } else { 60 }),
                        );
                } else if control.attack.is_some() {
                    control.attack = None;
                    after.ai.attack_cooldown = before.ai.attack_cooldown.saturating_sub(1);
                }
            } else {
                control.attack = None;
            }
        }
        control.destination = self.route_destination(
            world.world,
            &after,
            control.destination,
            now,
            &mut budget.snapshot_cells,
            work,
        )?;
        let body = movement::plan_body(&after, world, control, &mut self.bufs)?;
        after = body.after;
        if body.outcome.fall_damage > 0 {
            after.life.damage(
                DamageKind::Fall,
                body.outcome.fall_damage as f32,
                (after.kind.definition().armor, 0.0),
                false,
            );
        }
        Ok((after, control))
    }
    fn players_snapshot_target(&self, id: EntityId) -> Option<PlayerProbe> {
        self.players.get(id)
    }
    fn route_destination(
        &mut self,
        world: &World,
        mob: &Mob,
        destination: Option<[f64; 3]>,
        now: Tick,
        snapshot_budget: &mut u64,
        work: &mut Work,
    ) -> Result<Option<[f64; 3]>, EntityError> {
        let Some(destination) = destination else {
            self.routes.remove(&mob.id);
            self.paths.cancel(mob.id);
            return Ok(None);
        };
        if (0..3)
            .map(|axis| (destination[axis] - mob.body.position[axis]).powi(2))
            .sum::<f64>()
            < 0.09
        {
            return Ok(None);
        }
        let goal = destination.map(|value| value.floor() as i32);
        if self.routes.get(&mob.id).is_some_and(|route| {
            route.goal != goal || !PathJobs::inputs_match(world, &route.inputs)
        }) {
            self.routes.remove(&mob.id);
        }
        if let Some(PathResult {
            path: Some(path),
            goal: ready_goal,
            inputs,
            ..
        }) = self.paths.take_ready(mob.id)
            && ready_goal == goal
            && PathJobs::inputs_match(world, &inputs)
        {
            self.routes.insert(
                mob.id,
                Route {
                    path,
                    index: 1,
                    goal,
                    inputs,
                },
            );
        }
        if let Some(route) = self.routes.get_mut(&mob.id) {
            while route.index < route.path.points().len() {
                let target = route.path.points()[route.index];
                let point = [
                    f64::from(target[0]) + 0.5,
                    f64::from(target[1]),
                    f64::from(target[2]) + 0.5,
                ];
                if (point[0] - mob.body.position[0]).abs() < 0.35
                    && (point[2] - mob.body.position[2]).abs() < 0.35
                    && (point[1] - mob.body.position[1]).abs() < 0.6
                {
                    route.index += 1;
                } else {
                    return Ok(Some(point));
                }
            }
            self.routes.remove(&mob.id);
        }
        work.waiting_path += 1;
        match self.paths.request(world, mob, goal, now, snapshot_budget) {
            Ok(_) | Err(EntityError::Capacity) => {}
            Err(error) => return Err(error),
        }
        Ok(None)
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            seed: self.seed,
            revision: self.revision,
            entities: self.store.snapshot(),
        }
    }
    pub const fn capacity(&self) -> usize {
        self.maximum
    }
    pub fn restore(config: Config, snapshot: &Snapshot) -> Result<Self, EntityError> {
        let mut runtime = Self::new(Config {
            seed: snapshot.seed,
            ..config
        })?;
        runtime.store = EntityStore::restore(config.maximum_entities, &snapshot.entities)?;
        runtime.revision = snapshot.revision;
        Ok(runtime)
    }
}
fn known_push_body(
    world: &World,
    mob: Mob,
    budget: &mut rules::entities::spatial::QueryBudget,
    inputs: &mut BTreeMap<PageId, Option<Version>>,
    work: &mut Work,
) -> bool {
    let bounds = mob.bbox();
    let low = bounds.lo.map(|v| (v.floor() as i32).div_euclid(16));
    let high = bounds.hi.map(|v| (v.ceil() as i32 - 1).div_euclid(16));
    for z in low[2]..=high[2] {
        for y in low[1]..=high[1] {
            for x in low[0]..=high[0] {
                if budget.probes == 0 {
                    return false;
                }
                budget.probes -= 1;
                work.push_probes += 1;
                let page = PageId {
                    space: mob.space,
                    x,
                    y,
                    z,
                };
                if world.page(page).is_none() {
                    return false;
                }
                inputs.insert(page, Some(world.input_version(page, CHANNELS)));
            }
        }
    }
    true
}
fn charge_push(
    query: rules::entities::spatial::QueryWork,
    budget: &mut rules::entities::spatial::QueryBudget,
    work: &mut Work,
) {
    budget.buckets = budget.buckets.saturating_sub(query.buckets);
    budget.probes = budget.probes.saturating_sub(query.probes);
    budget.candidates = budget.candidates.saturating_sub(query.candidates);
    work.push_candidates += query.candidates;
    work.push_buckets += query.buckets;
    work.push_probes += query.probes;
    work.push_deferred += u64::from(query.overflow);
}
struct Tracked<'a> {
    world: &'a World,
    space: SpaceId,
    reads: &'a Counter<u64>,
    maximum: u64,
    exhausted: &'a Counter<bool>,
    inputs: &'a RefCell<BTreeMap<PageId, Option<Version>>>,
}
impl QueryWorld for Tracked<'_> {
    fn cell(&self, p: [i32; 3]) -> CellRead {
        if self.reads.get() >= self.maximum {
            self.exhausted.set(true);
            return CellRead::Unknown;
        }
        self.reads.set(self.reads.get() + 1);
        let position = BlockPos {
            space: self.space,
            x: p[0],
            y: p[1],
            z: p[2],
        };
        let version = self
            .world
            .page(position.page())
            .map(|_| self.world.input_version(position.page(), CHANNELS));
        let mut inputs = self.inputs.borrow_mut();
        if inputs.len() >= 4096 && !inputs.contains_key(&position.page()) {
            self.exhausted.set(true);
            return CellRead::Unknown;
        }
        inputs.insert(position.page(), version);
        self.world.read(position)
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub seed: u64,
    pub revision: u64,
    pub entities: EntitySnapshot,
}
impl Snapshot {
    pub fn encode(&self) -> Result<Vec<u8>, EntityError> {
        let entities = self.entities.encode()?;
        let mut bytes = Vec::with_capacity(24 + entities.len());
        bytes.extend_from_slice(b"MOR1");
        bytes.extend_from_slice(&self.seed.to_le_bytes());
        bytes.extend_from_slice(&self.revision.to_le_bytes());
        bytes.extend_from_slice(&(entities.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&entities);
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], maximum_entities: usize) -> Result<Self, EntityError> {
        if bytes.len() < 24 || &bytes[..4] != b"MOR1" {
            return Err(EntityError::InvalidEncoding);
        }
        let length = u32::from_le_bytes(
            bytes[20..24]
                .try_into()
                .map_err(|_| EntityError::InvalidEncoding)?,
        ) as usize;
        if length.checked_add(24) != Some(bytes.len()) {
            return Err(EntityError::InvalidEncoding);
        }
        Ok(Self {
            seed: u64::from_le_bytes(
                bytes[4..12]
                    .try_into()
                    .map_err(|_| EntityError::InvalidEncoding)?,
            ),
            revision: u64::from_le_bytes(
                bytes[12..20]
                    .try_into()
                    .map_err(|_| EntityError::InvalidEncoding)?,
            ),
            entities: EntitySnapshot::decode(&bytes[24..], maximum_entities)?,
        })
    }
}
impl MobDelta {
    pub fn encode(&self) -> Result<Vec<u8>, EntityError> {
        if self.patches.len() > 1_000_000
            || self.cursor_before > 1_000_000
            || self.cursor_after > 1_000_000
        {
            return Err(EntityError::Capacity);
        }
        let mut bytes = Vec::with_capacity(32 + self.patches.len() * 644);
        bytes.extend_from_slice(b"MOD1");
        bytes.extend_from_slice(&self.revision.to_le_bytes());
        bytes.extend_from_slice(&self.next_revision.to_le_bytes());
        for value in [self.cursor_before, self.cursor_after, self.patches.len()] {
            bytes.extend_from_slice(&(value as u32).to_le_bytes());
        }
        for patch in &self.patches {
            patch.id()?;
            bytes.extend_from_slice(&[
                u8::from(patch.before.is_some())
                    | (u8::from(patch.after.is_some()) << 1)
                    | (u8::from(patch.before_active) << 2)
                    | (u8::from(patch.after_active) << 3),
                0,
                0,
                0,
            ]);
            if let Some(before) = patch.before {
                bytes.extend_from_slice(&before.encode()?);
            }
            if let Some(after) = patch.after {
                bytes.extend_from_slice(&after.encode()?);
            }
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], maximum_changes: usize) -> Result<Self, EntityError> {
        if bytes.len() < 32 || &bytes[..4] != b"MOD1" {
            return Err(EntityError::InvalidEncoding);
        }
        let read32 = |index| {
            u32::from_le_bytes(bytes[index..index + 4].try_into().expect("header length")) as usize
        };
        let count = read32(28);
        if count > maximum_changes {
            return Err(EntityError::Capacity);
        }
        let mut cursor = 32usize;
        let mut patches = Vec::with_capacity(count);
        for _ in 0..count {
            let header = bytes
                .get(cursor..cursor + 4)
                .ok_or(EntityError::InvalidEncoding)?;
            cursor += 4;
            let flags = header[0];
            if flags & !15 != 0 || header[1..].iter().any(|&value| value != 0) || flags & 3 == 0 {
                return Err(EntityError::InvalidEncoding);
            }
            let mut values = [None; 2];
            for (index, value) in values.iter_mut().enumerate() {
                if flags & (1 << index) != 0 {
                    let end = cursor
                        .checked_add(mob::MOB_BYTES)
                        .ok_or(EntityError::InvalidEncoding)?;
                    *value = Some(Mob::decode(
                        bytes.get(cursor..end).ok_or(EntityError::InvalidEncoding)?,
                    )?);
                    cursor = end;
                }
            }
            let patch = EntityPatch {
                before: values[0],
                after: values[1],
                before_active: flags & 4 != 0,
                after_active: flags & 8 != 0,
            };
            patch.id()?;
            if (patch.before.is_none() && patch.before_active)
                || (patch.after.is_none() && patch.after_active)
            {
                return Err(EntityError::InvalidEncoding);
            }
            patches.push(patch);
        }
        if cursor != bytes.len() {
            return Err(EntityError::InvalidEncoding);
        }
        Ok(Self {
            revision: u64::from_le_bytes(bytes[4..12].try_into().expect("header length")),
            next_revision: u64::from_le_bytes(bytes[12..20].try_into().expect("header length")),
            cursor_before: read32(20),
            cursor_after: read32(24),
            patches,
        })
    }
}

fn current_mob(
    store: &EntityStore,
    staged: &BTreeMap<EntityId, EntityPatch>,
    id: EntityId,
) -> Option<Mob> {
    staged
        .get(&id)
        .map_or_else(|| store.get(id).copied(), |patch| patch.after)
}
