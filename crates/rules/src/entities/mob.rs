//! Dense entity ownership, spatial player queries and deterministic AI intents.
//! World motion/navigation are explicit adapters; no per-frame vertices exist.
use super::{
    EntityError, EntityId, MobKind,
    spatial::{QueryBudget, SpatialHash},
};
mod codec;
use crate::{
    ids::*,
    items::*,
    stack::ItemStack,
    survival::{health::Health, loot::unit},
};
pub use codec::{EntitySnapshot, MOB_BYTES};
use core_types::{DeterministicRng, SpaceId, Tick};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body {
    pub position: [f64; 3],
    pub previous: [f64; 3],
    pub velocity: [f64; 3],
    pub yaw: f32,
    pub head: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub in_water: bool,
    pub in_lava: bool,
    pub fall: f32,
    pub jump_cooldown: u8,
    pub horizontal_collision: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Goal {
    Idle,
    Swim,
    Panic,
    Tempt,
    EatGrass,
    Wander,
    Melee,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AiState {
    pub goal: Goal,
    pub target: Option<EntityId>,
    pub destination: Option<[f64; 3]>,
    pub goal_until: Tick,
    pub panic_until: Tick,
    pub attack_cooldown: u16,
    pub eating: u8,
    pub event: u64,
    pub speed_multiplier: f32,
}
impl Default for AiState {
    fn default() -> Self {
        Self {
            goal: Goal::Idle,
            target: None,
            destination: None,
            goal_until: Tick(0),
            panic_until: Tick(0),
            attack_cooldown: 0,
            eating: 0,
            event: 0,
            speed_multiplier: 1.0,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mob {
    pub id: EntityId,
    pub space: SpaceId,
    pub kind: MobKind,
    pub body: Body,
    pub life: Health,
    pub ai: AiState,
    pub age: u64,
    pub last_tick: Tick,
    pub loot_emitted: bool,
    pub baby: bool,
    pub sheared: bool,
    pub color: u8,
    pub limb_phase: f32,
    pub limb_amount: f32,
}
impl Mob {
    pub fn new(id: EntityId, kind: MobKind, position: [f64; 3]) -> Result<Self, EntityError> {
        validate_position(position)?;
        let mut life = Health::with_maximum(kind.definition().health)
            .map_err(|_| EntityError::InvalidEncoding)?;
        life.set_spawn_protection(0);
        Ok(Self {
            id,
            space: SpaceId(0),
            kind,
            body: Body {
                position,
                previous: position,
                velocity: [0.0, -0.0784, 0.0],
                yaw: 0.0,
                head: 0.0,
                pitch: 0.0,
                on_ground: true,
                in_water: false,
                in_lava: false,
                fall: 0.0,
                jump_cooldown: 0,
                horizontal_collision: false,
            },
            life,
            ai: AiState::default(),
            age: 0,
            last_tick: Tick(0),
            loot_emitted: false,
            baby: false,
            sheared: false,
            color: 0,
            limb_phase: 0.0,
            limb_amount: 0.0,
        })
    }
    pub fn new_in_space(
        id: EntityId,
        kind: MobKind,
        space: SpaceId,
        position: [f64; 3],
    ) -> Result<Self, EntityError> {
        Ok(Self {
            space,
            ..Self::new(id, kind, position)?
        })
    }
    pub fn dimensions(&self) -> (f64, f64) {
        let definition = self.kind.definition();
        let scale = if self.baby { 0.5 } else { 1.0 };
        (definition.width * scale, definition.height * scale)
    }
    pub fn bbox(&self) -> crate::shapes::Aabb {
        let (width, height) = self.dimensions();
        let [x, y, z] = self.body.position;
        crate::shapes::Aabb {
            lo: [x - width / 2.0, y, z - width / 2.0],
            hi: [x + width / 2.0, y + height, z + width / 2.0],
        }
    }
    pub fn begin_tick(&mut self) {
        self.body.previous = self.body.position;
        self.age = self.age.saturating_add(1);
        self.life.begin_tick();
        self.ai.attack_cooldown = self.ai.attack_cooldown.saturating_sub(1);
    }
    pub fn finish_pose(&mut self) {
        let delta = (self.body.position[0] - self.body.previous[0]).powi(2)
            + (self.body.position[2] - self.body.previous[2]).powi(2);
        let target = (delta.sqrt() * 4.0).min(1.0) as f32;
        self.limb_amount += (target - self.limb_amount) * 0.4;
        self.limb_phase += self.limb_amount;
    }
    /// The adapter must accept/reject this intent against known collision/path
    /// snapshots; a destination is never permission to walk through a block.
    pub fn plan_ai(
        &self,
        now: Tick,
        seed: u64,
        players: &PlayerIndex,
        environment: Environment,
        budget: &mut AiBudget,
    ) -> Result<(Self, Control), EntityError> {
        let mut mob = *self;
        let mut control = Control::default();
        if !mob.life.alive() {
            return Ok((mob, control));
        }
        let mut rng = DeterministicRng::from_key(seed, 0x4d4f424149, mob.id.0, mob.ai.event);
        mob.ai.event = mob.ai.event.checked_add(1).ok_or(EntityError::Capacity)?;
        if mob.kind != MobKind::Zombie && (mob.body.in_water || mob.body.in_lava) {
            control.jump = unit(&mut rng) < 0.8;
        }
        if mob.ai.panic_until >= now && mob.ai.panic_until.0 > 0 && mob.kind != MobKind::Zombie {
            mob.ai.goal = Goal::Panic;
            if self.ai.goal != Goal::Panic
                || mob.ai.destination.is_none()
                || mob.ai.goal_until <= now
            {
                mob.ai.destination = Some(random_destination(mob.body.position, &mut rng, 5));
                mob.ai.goal_until = Tick(now.0.saturating_add(40));
            }
            control.destination = mob.ai.destination;
            control.speed_multiplier = mob.kind.definition().panic;
            mob.ai.speed_multiplier = control.speed_multiplier;
            return Ok((mob, control));
        }
        let full = (mob.age.wrapping_add(mob.id.0)).is_multiple_of(2) || mob.age <= 1;
        if mob.kind == MobKind::Zombie {
            let target = players.nearest_in_space(
                mob.space,
                mob.body.position,
                mob.kind.definition().follow,
                budget,
                |player| player.alive && !player.creative && !player.spectator,
            )?;
            mob.ai.target = target.map(|player| player.id);
            if let Some(target) = target {
                mob.ai.goal = Goal::Melee;
                control.look_at = Some(target.eye);
                control.destination = Some(target.position);
                control.speed_multiplier = 1.0;
                let distance = (0..3)
                    .map(|axis| (target.position[axis] - mob.body.position[axis]).powi(2))
                    .sum::<f64>();
                if distance <= 2.04 && mob.ai.attack_cooldown == 0 && !target.protected {
                    control.attack = Some(target.id);
                    mob.ai.attack_cooldown = 20;
                }
            } else {
                mob.ai.goal = Goal::Idle;
                mob.ai.destination = None;
            }
        } else if full {
            let kind = mob.kind;
            let tempted =
                players.nearest_in_space(mob.space, mob.body.position, 10.0, budget, |player| {
                    player.alive
                        && !player.creative
                        && !player.spectator
                        && player.hands.iter().any(|&item| is_food(kind, item))
                })?;
            if let Some(player) = tempted {
                mob.ai.goal = Goal::Tempt;
                mob.ai.target = Some(player.id);
                control.look_at = Some(player.eye);
                let distance = (0..3)
                    .map(|axis| (player.position[axis] - mob.body.position[axis]).powi(2))
                    .sum::<f64>();
                if distance >= 6.25 {
                    control.destination = Some(player.position);
                    control.speed_multiplier = match mob.kind {
                        MobKind::Pig => 1.2,
                        MobKind::Cow => 1.25,
                        MobKind::Sheep => 1.1,
                        _ => 1.0,
                    };
                }
            } else if mob.kind == MobKind::Sheep
                && (mob.ai.eating > 0
                    || (environment.grass
                        && rng.below(if mob.baby { 50 } else { 1000 }) == Some(0)))
            {
                mob.ai.goal = Goal::EatGrass;
                if mob.ai.eating == 0 {
                    mob.ai.eating = 40;
                }
                mob.ai.eating = mob.ai.eating.saturating_sub(2);
                if mob.ai.eating == 4 {
                    control.eat_grass = true;
                    mob.sheared = false;
                }
            } else {
                if mob.ai.goal_until <= now {
                    mob.ai.destination = None;
                    mob.ai.goal = Goal::Idle;
                    if rng.below(60) == Some(0) {
                        mob.ai.destination =
                            Some(random_destination(mob.body.position, &mut rng, 10));
                        mob.ai.goal_until = Tick(now.0.saturating_add(100));
                        mob.ai.goal = Goal::Wander;
                    }
                }
                control.destination = mob.ai.destination;
                control.speed_multiplier = 1.0;
            }
        } else {
            control.destination = mob.ai.destination;
            control.speed_multiplier = mob.ai.speed_multiplier;
        }
        if control.look_at.is_none() && full && unit(&mut rng) < 0.02 {
            control.look_at = players
                .nearest_in_space(
                    mob.space,
                    mob.body.position,
                    mob.kind.definition().look_range,
                    budget,
                    |player| player.alive,
                )?
                .map(|player| player.eye);
        }
        let fraction = f32::from(
            environment
                .sky_light
                .min(15)
                .saturating_sub(environment.darkness),
        ) / 15.0;
        let brightness = fraction / (4.0 - 3.0 * fraction);
        if mob.kind == MobKind::Zombie
            && environment.sky_visible
            && !mob.body.in_water
            && environment.sky_light == 15
            && environment.darkness < 4
            && brightness > 0.5
            && unit(&mut rng) * 30.0 < (brightness - 0.4) * 2.0
        {
            mob.life.ignite(160);
        }
        mob.ai.speed_multiplier = control.speed_multiplier;
        mob.ai.destination = control.destination;
        Ok((mob, control))
    }
}
pub fn validate_position(position: [f64; 3]) -> Result<(), EntityError> {
    if position.iter().any(|&value| {
        !value.is_finite()
            || value < f64::from(i32::MIN) + 1024.0
            || value > f64::from(i32::MAX) - 1024.0
    }) {
        Err(EntityError::InvalidPosition)
    } else {
        Ok(())
    }
}
fn random_destination(position: [f64; 3], rng: &mut DeterministicRng, radius: i32) -> [f64; 3] {
    let mut target = position;
    for axis in [0, 2] {
        target[axis] +=
            rng.below((radius * 2 + 1) as u64).expect("positive radius") as f64 - f64::from(radius);
    }
    target
}
fn is_food(kind: MobKind, item: u16) -> bool {
    match kind {
        MobKind::Pig => matches!(item, I_CARROT | I_POTATO),
        MobKind::Cow | MobKind::Sheep => item == I_WHEAT,
        MobKind::Chicken => item == I_WHEAT_SEEDS,
        MobKind::Zombie => false,
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Environment {
    pub grass: bool,
    pub sky_visible: bool,
    pub sky_light: u8,
    pub darkness: u8,
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Control {
    pub destination: Option<[f64; 3]>,
    pub speed_multiplier: f32,
    pub jump: bool,
    pub look_at: Option<[f64; 3]>,
    pub attack: Option<EntityId>,
    pub eat_grass: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct AiBudget {
    pub buckets: u64,
    pub probes: u64,
    pub candidates: u64,
}
impl Default for AiBudget {
    fn default() -> Self {
        Self {
            buckets: 16_384,
            probes: 65_536,
            candidates: 4096,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerProbe {
    pub id: EntityId,
    pub space: SpaceId,
    pub position: [f64; 3],
    pub eye: [f64; 3],
    pub hands: [u16; 2],
    pub alive: bool,
    pub creative: bool,
    pub spectator: bool,
    pub protected: bool,
}
#[derive(Debug)]
pub struct PlayerIndex {
    players: Vec<PlayerProbe>,
    spatial: SpatialHash,
    capacity: usize,
}
impl PlayerIndex {
    pub fn new(capacity: usize) -> Result<Self, EntityError> {
        Ok(Self {
            players: Vec::with_capacity(capacity),
            spatial: SpatialHash::new(capacity, 8.0)?,
            capacity,
        })
    }
    pub fn get(&self, id: EntityId) -> Option<PlayerProbe> {
        self.players.iter().copied().find(|player| player.id == id)
    }
    pub fn rebuild(&mut self, players: &[PlayerProbe]) -> Result<(), EntityError> {
        if players.len() > self.capacity {
            return Err(EntityError::Capacity);
        }
        for player in players {
            validate_position(player.position)?;
            validate_position(player.eye)?;
        }
        for index in players.len()..self.players.len() {
            self.spatial.remove(index)?;
        }
        self.players.clear();
        self.players.extend_from_slice(players);
        for (index, player) in self.players.iter().enumerate() {
            self.spatial.update(index, player.position)?;
        }
        Ok(())
    }
    pub fn nearest(
        &self,
        position: [f64; 3],
        radius: f64,
        budget: &mut AiBudget,
        filter: impl Fn(&PlayerProbe) -> bool,
    ) -> Result<Option<PlayerProbe>, EntityError> {
        self.nearest_in_space(SpaceId(0), position, radius, budget, filter)
    }
    pub fn nearest_in_space(
        &self,
        space: SpaceId,
        position: [f64; 3],
        radius: f64,
        budget: &mut AiBudget,
        filter: impl Fn(&PlayerProbe) -> bool,
    ) -> Result<Option<PlayerProbe>, EntityError> {
        if self.players.is_empty() {
            return Ok(None);
        }
        if !radius.is_finite() || radius < 0.0 {
            return Err(EntityError::InvalidPosition);
        }
        let mut best: Option<(f64, PlayerProbe)> = None;
        let work = self.spatial.query(
            position.map(|v| v - radius),
            position.map(|v| v + radius),
            QueryBudget {
                buckets: budget.buckets,
                probes: budget.probes,
                candidates: budget.candidates,
            },
            |index| {
                let player = self.players[index];
                if player.space != space || !filter(&player) {
                    return;
                }
                let distance = (0..3)
                    .map(|axis| (player.position[axis] - position[axis]).powi(2))
                    .sum::<f64>();
                if distance <= radius * radius
                    && best.is_none_or(|(old, existing)| {
                        distance < old || (distance == old && player.id < existing.id)
                    })
                {
                    best = Some((distance, player));
                }
            },
        )?;
        budget.buckets = budget.buckets.saturating_sub(work.buckets);
        budget.probes = budget.probes.saturating_sub(work.probes);
        budget.candidates = budget.candidates.saturating_sub(work.candidates);
        if work.overflow {
            return Err(EntityError::Capacity);
        }
        Ok(best.map(|(_, player)| player))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct Instance {
    pub position: [f32; 3],
    pub body_yaw: f32,
    pub head_yaw: f32,
    pub pitch: f32,
    pub limb_phase: f32,
    pub limb_amount: f32,
    pub kind: u32,
    pub flags: u32,
    pub color: u32,
    pub id_low: u32,
    pub id_high: u32,
    pub death_roll: f32,
    pub scale: f32,
    pub reserved: f32,
}
pub const INSTANCE_BYTES: usize = 64;
pub struct EntityStore {
    rows: Vec<Mob>,
    ids: BTreeMap<EntityId, usize>,
    active: Vec<usize>,
    active_slots: Vec<Option<usize>>,
    spatial: SpatialHash,
    capacity: usize,
    cursor: usize,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntityPatch {
    pub before: Option<Mob>,
    pub after: Option<Mob>,
    pub before_active: bool,
    pub after_active: bool,
}
impl EntityPatch {
    pub fn id(&self) -> Result<EntityId, EntityError> {
        match (self.before, self.after) {
            (Some(before), Some(after)) if before.id == after.id => Ok(before.id),
            (Some(before), None) => Ok(before.id),
            (None, Some(after)) => Ok(after.id),
            _ => Err(EntityError::InvalidEncoding),
        }
    }
}
impl EntityStore {
    pub fn new(capacity: usize) -> Result<Self, EntityError> {
        Ok(Self {
            rows: Vec::with_capacity(capacity),
            ids: BTreeMap::new(),
            active: Vec::with_capacity(capacity),
            active_slots: Vec::with_capacity(capacity),
            spatial: SpatialHash::new(capacity, 8.0)?,
            capacity,
            cursor: 0,
        })
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn active_len(&self) -> usize {
        self.active.len()
    }
    pub fn get(&self, id: EntityId) -> Option<&Mob> {
        self.ids.get(&id).map(|&index| &self.rows[index])
    }
    pub fn rows(&self) -> &[Mob] {
        &self.rows
    }
    pub fn is_active(&self, id: EntityId) -> bool {
        self.ids
            .get(&id)
            .is_some_and(|&index| self.active_slots[index].is_some())
    }
    pub fn validate_patches(&self, patches: &[EntityPatch]) -> Result<(), EntityError> {
        let mut seen = std::collections::BTreeSet::new();
        let mut count = self.len();
        for patch in patches {
            let id = patch.id()?;
            if !seen.insert(id)
                || self.get(id).copied() != patch.before
                || self.is_active(id) != patch.before_active
                || (patch.before.is_none() && patch.before_active)
                || (patch.after.is_none() && patch.after_active)
            {
                return Err(EntityError::StalePlan);
            }
            if let Some(after) = patch.after {
                if patch.before.is_some_and(|before| before.kind != after.kind) {
                    return Err(EntityError::StalePlan);
                }
                after.encode()?;
                if patch.before.is_none() {
                    count = count.checked_add(1).ok_or(EntityError::Capacity)?;
                }
            } else {
                count = count.checked_sub(1).ok_or(EntityError::InvalidEncoding)?;
            }
        }
        if count > self.capacity {
            return Err(EntityError::Capacity);
        }
        Ok(())
    }
    /// Validate the complete ledger delta before publishing any entity row.
    pub fn apply_patches(&mut self, patches: &[EntityPatch]) -> Result<(), EntityError> {
        self.validate_patches(patches)?;
        // Removals first release capacity for accepted replacements/spawns.
        for patch in patches {
            if patch.after.is_none() {
                self.remove(patch.id()?)?;
            }
        }
        for patch in patches {
            if let Some(after) = patch.after {
                if let Some(before) = patch.before {
                    self.replace(&before, after)?;
                    self.set_active(after.id, patch.after_active)?;
                } else {
                    self.insert(after, patch.after_active)?;
                }
            }
        }
        Ok(())
    }
    pub fn insert(&mut self, mob: Mob, active: bool) -> Result<(), EntityError> {
        if self.rows.len() == self.capacity {
            return Err(EntityError::Capacity);
        }
        if self.ids.contains_key(&mob.id) {
            return Err(EntityError::DuplicateId);
        }
        let index = self.rows.len();
        self.spatial.update(index, mob.body.position)?;
        self.ids.insert(mob.id, index);
        self.rows.push(mob);
        self.active_slots.push(None);
        self.set_active(mob.id, active)?;
        Ok(())
    }
    pub fn replace(&mut self, expected: &Mob, after: Mob) -> Result<(), EntityError> {
        let &index = self.ids.get(&expected.id).ok_or(EntityError::UnknownId)?;
        if self.rows[index] != *expected || after.id != expected.id || after.kind != expected.kind {
            return Err(EntityError::StalePlan);
        }
        validate_position(after.body.position)?;
        self.spatial.update(index, after.body.position)?;
        self.rows[index] = after;
        Ok(())
    }
    pub fn set_active(&mut self, id: EntityId, active: bool) -> Result<(), EntityError> {
        let &index = self.ids.get(&id).ok_or(EntityError::UnknownId)?;
        match (self.active_slots[index], active) {
            (None, true) => {
                self.active_slots[index] = Some(self.active.len());
                self.active.push(index);
            }
            (Some(slot), false) => {
                self.active.swap_remove(slot);
                if slot < self.active.len() {
                    self.active_slots[self.active[slot]] = Some(slot);
                }
                self.active_slots[index] = None;
            }
            _ => {}
        }
        Ok(())
    }
    pub fn remove(&mut self, id: EntityId) -> Result<Mob, EntityError> {
        let &index = self.ids.get(&id).ok_or(EntityError::UnknownId)?;
        self.set_active(id, false)?;
        self.ids.remove(&id);
        let last = self.rows.len() - 1;
        self.spatial.remove(index)?;
        if index != last {
            self.spatial.move_index(last, index)?;
        }
        let removed = self.rows.swap_remove(index);
        self.active_slots.swap_remove(index);
        if index < self.rows.len() {
            self.ids.insert(self.rows[index].id, index);
            if let Some(slot) = self.active_slots[index] {
                self.active[slot] = index;
            }
        }
        if self.cursor >= self.active.len() {
            self.cursor = 0;
        }
        Ok(removed)
    }
    /// Only caller-selected visible IDs are converted to constant-size instances.
    /// No cuboid, vertex, bone matrix or trigonometric evaluation happens on CPU.
    pub fn instances(
        &self,
        visible: &[EntityId],
        alpha: f64,
        out: &mut [Instance],
    ) -> Result<usize, EntityError> {
        self.instances_at(visible, alpha, [0; 3], out)
    }
    pub fn instances_at(
        &self,
        visible: &[EntityId],
        alpha: f64,
        origin: [i32; 3],
        out: &mut [Instance],
    ) -> Result<usize, EntityError> {
        if visible.len() > out.len() || !alpha.is_finite() {
            return Err(EntityError::Capacity);
        }
        let alpha = alpha.clamp(0.0, 1.0);
        for (&id, instance) in visible.iter().zip(out.iter_mut()) {
            let mob = self.get(id).ok_or(EntityError::UnknownId)?;
            *instance = Instance {
                position: std::array::from_fn(|axis| {
                    (mob.body.previous[axis] - f64::from(origin[axis])
                        + (mob.body.position[axis] - mob.body.previous[axis]) * alpha)
                        as f32
                }),
                body_yaw: mob.body.yaw,
                head_yaw: mob.body.head,
                pitch: mob.body.pitch,
                limb_phase: mob.limb_phase,
                limb_amount: mob.limb_amount,
                kind: mob.kind as u32,
                flags: u32::from(mob.baby)
                    | (u32::from(mob.sheared) << 1)
                    | (u32::from(!mob.life.alive()) << 2)
                    | (u32::from(mob.life.fire() > 0) << 3)
                    | (u32::from(mob.life.hurt_time() > 0) << 4),
                color: u32::from(mob.color),
                id_low: id.0 as u32,
                id_high: (id.0 >> 32) as u32,
                death_roll: if mob.life.alive() {
                    0.0
                } else {
                    (f32::from(mob.life.death_time()) / 20.0).min(1.0) * 90.0
                },
                scale: if mob.baby { 0.5 } else { 1.0 },
                reserved: 0.0,
            };
        }
        Ok(visible.len())
    }
    pub fn query_ids(
        &self,
        low: [f64; 3],
        high: [f64; 3],
        budget: QueryBudget,
        out: &mut [EntityId],
    ) -> Result<(usize, super::spatial::QueryWork), EntityError> {
        let mut count = 0;
        let mut overflow = false;
        let mut work = self.spatial.query(low, high, budget, |index| {
            if count == out.len() {
                overflow = true;
            } else {
                out[count] = self.rows[index].id;
                count += 1;
            }
        })?;
        work.overflow |= overflow;
        Ok((count, work))
    }
    /// Stable rotating admission prevents a busy area starving later active IDs.
    pub fn active_batch(&mut self, maximum: usize, out: &mut [EntityId]) -> usize {
        let count = maximum.min(out.len()).min(self.active.len());
        for target in out.iter_mut().take(count) {
            *target = self.rows[self.active[self.cursor]].id;
            self.cursor = (self.cursor + 1) % self.active.len();
        }
        count
    }
    pub const fn active_cursor(&self) -> usize {
        self.cursor
    }
    pub fn peek_active_batch(&self, maximum: usize, out: &mut [EntityId]) -> (usize, usize) {
        let count = maximum.min(out.len()).min(self.active.len());
        let mut cursor = self.cursor;
        if !self.active.is_empty() {
            cursor %= self.active.len();
        }
        for target in out.iter_mut().take(count) {
            *target = self.rows[self.active[cursor]].id;
            cursor = (cursor + 1) % self.active.len();
        }
        (count, cursor)
    }
    pub fn publish_active_cursor(
        &mut self,
        expected: usize,
        next: usize,
    ) -> Result<(), EntityError> {
        if self.cursor != expected
            || (!self.active.is_empty() && next >= self.active.len())
            || (self.active.is_empty() && next != 0)
        {
            return Err(EntityError::StalePlan);
        }
        self.cursor = next;
        Ok(())
    }
}

pub fn mob_loot(mob: &Mob, by_player: bool, rng: &mut DeterministicRng) -> [ItemStack; 3] {
    let mut out = [ItemStack::EMPTY; 3];
    if mob.baby && mob.kind != MobKind::Zombie {
        return out;
    }
    let fire = mob.life.fire() > 0;
    let meat = |raw, cooked| if fire { cooked } else { raw };
    let mut index = 0;
    let mut add = |id, count| {
        if count > 0 && id != 0 {
            out[index] = ItemStack::new(id, count).expect("bounded mob loot");
            index += 1;
        }
    };
    let range = |rng: &mut DeterministicRng, min: u16, max: u16| {
        min + rng.below(u64::from(max - min) + 1).expect("nonempty range") as u16
    };
    match mob.kind {
        MobKind::Pig => add(meat(I_PORKCHOP, I_COOKED_PORKCHOP), range(rng, 1, 3)),
        MobKind::Cow => {
            add(I_LEATHER, range(rng, 0, 2));
            add(meat(I_BEEF, I_COOKED_BEEF), range(rng, 1, 3));
        }
        MobKind::Sheep => {
            if !mob.sheared {
                add(WHITE_WOOL + u16::from(mob.color), 1);
            }
            add(meat(I_MUTTON, I_COOKED_MUTTON), range(rng, 1, 2));
        }
        MobKind::Chicken => {
            add(I_FEATHER, range(rng, 0, 2));
            add(meat(I_CHICKEN, I_COOKED_CHICKEN), 1);
        }
        MobKind::Zombie => {
            add(I_ROTTEN_FLESH, range(rng, 0, 2));
            if by_player && unit(rng) < 0.025 {
                add(
                    match rng.below(3).expect("three drops") {
                        0 => I_IRON_INGOT,
                        1 => I_CARROT,
                        _ => {
                            if fire {
                                I_BAKED_POTATO
                            } else {
                                I_POTATO
                            }
                        }
                    },
                    1,
                );
            }
        }
    }
    out
}
