//! Per-peer public AOI images. Only already committed owner stores are read;
//! confirmed revision history is independent from the latest sent image.
use core_types::{SpaceId, Tick};
use net::entities::{EntityFrame, EntityPose, EntityRequest, MAX_ENTITIES};
use rules::entities::{
    EntityError, EntityId,
    item::ItemStore,
    mob::{EntityStore, Instance},
    spatial::{QueryBudget, QueryWork},
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug)]
pub struct Interest {
    pub space: SpaceId,
    pub center: [f64; 3],
    pub enter: f64,
    pub retain: f64,
}
impl Interest {
    fn valid(self) -> bool {
        self.center
            .iter()
            .all(|v| v.is_finite() && v.abs() < 2_000_000_000.0)
            && self.enter.is_finite()
            && self.retain.is_finite()
            && self.enter > 0.0
            && self.enter <= self.retain
            && self.retain <= 256.0
    }
    fn includes(self, pose: EntityPose, retained: bool) -> bool {
        let radius = if retained { self.retain } else { self.enter };
        pose.space == self.space
            && (pose.position[1] - self.center[1]).abs() <= radius
            && (pose.position[0] - self.center[0]).powi(2)
                + (pose.position[2] - self.center[2]).powi(2)
                <= radius * radius
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub buckets: u64,
    pub probes: u64,
    pub candidates: u64,
    pub poses: u64,
    pub upserts: u64,
    pub removes: u64,
    pub full: bool,
}
#[derive(Clone)]
struct Image {
    revision: u64,
    poses: BTreeMap<u64, EntityPose>,
}
pub struct EntityHistory {
    images: VecDeque<Image>,
    visible: BTreeSet<u64>,
    confirmed: Option<u64>,
    next_revision: u64,
    maximum: usize,
    candidates: Vec<EntityId>,
    versions: usize,
    force_full: bool,
}
impl EntityHistory {
    pub fn new(
        maximum: usize,
        maximum_candidates: usize,
        versions: usize,
    ) -> Result<Self, EntityError> {
        if maximum == 0
            || maximum > MAX_ENTITIES
            || maximum_candidates < maximum
            || maximum_candidates > 65_536
            || !(1..=8).contains(&versions)
        {
            return Err(EntityError::Capacity);
        }
        Ok(Self {
            images: VecDeque::with_capacity(versions),
            visible: BTreeSet::new(),
            confirmed: None,
            next_revision: 1,
            maximum,
            candidates: vec![EntityId(0); maximum_candidates],
            versions,
            force_full: false,
        })
    }
    pub const fn confirmed(&self) -> Option<u64> {
        self.confirmed
    }
    pub fn request(&mut self, request: EntityRequest) -> bool {
        match request {
            EntityRequest::Ack { revision } => {
                if self.confirmed.is_some_and(|old| revision < old)
                    || !self.images.iter().any(|image| image.revision == revision)
                {
                    return false;
                }
                self.confirmed = Some(revision);
                true
            }
            EntityRequest::Full { .. } => {
                self.force_full = true;
                true
            }
        }
    }
    /// Failure leaves history/hysteresis unchanged; source overflows never
    /// become a partial full image that silently despawns nearby actors.
    pub fn plan(
        &mut self,
        mobs: &EntityStore,
        items: &ItemStore,
        interest: Interest,
        tick: Tick,
        mut budget: QueryBudget,
    ) -> Result<(Option<EntityFrame>, Work), EntityError> {
        if !interest.valid() {
            return Err(EntityError::InvalidPosition);
        }
        let low = interest.center.map(|v| v - interest.retain);
        let high = interest.center.map(|v| v + interest.retain);
        let mut poses = BTreeMap::new();
        let mut work = Work::default();
        let mut instance = [Instance::default(); 1];
        if !mobs.is_empty() {
            let (count, query) = mobs.query_ids(low, high, budget, &mut self.candidates)?;
            charge(query, &mut budget, &mut work)?;
            for &id in &self.candidates[..count] {
                let mob = mobs.get(id).ok_or(EntityError::StalePlan)?;
                mobs.instances_at(&[id], 1.0, [0; 3], &mut instance)?;
                let pose = public_pose(id, mob.space, mob.body.position, instance[0], 0, 0)?;
                if interest.includes(pose, self.visible.contains(&id.0)) {
                    poses.insert(id.0, pose);
                }
            }
        }
        if !items.is_empty() {
            let (count, query) = items.query_ids(low, high, budget, &mut self.candidates)?;
            charge(query, &mut budget, &mut work)?;
            for &id in &self.candidates[..count] {
                let item = items.get(id).ok_or(EntityError::StalePlan)?;
                items.instances_at(&[id], 1.0, [0; 3], &mut instance)?;
                let pose = public_pose(
                    id,
                    item.space,
                    item.position,
                    instance[0],
                    item.stack.count(),
                    item.stack.damage(),
                )?;
                if interest.includes(pose, self.visible.contains(&id.0))
                    && poses.insert(id.0, pose).is_some()
                {
                    return Err(EntityError::DuplicateId);
                }
            }
        }
        if poses.len() > self.maximum {
            return Err(EntityError::Capacity);
        }
        work.poses = poses.len() as u64;
        let base = self
            .confirmed
            .and_then(|revision| self.images.iter().find(|image| image.revision == revision));
        let base = if self.force_full { None } else { base };
        let (base_revision, upserts, removes) = if let Some(base) = base {
            let upserts: Vec<_> = poses
                .iter()
                .filter(|(id, pose)| base.poses.get(id) != Some(pose))
                .map(|(_, pose)| *pose)
                .collect();
            let removes: Vec<_> = base
                .poses
                .keys()
                .filter(|id| !poses.contains_key(id))
                .copied()
                .collect();
            (Some(base.revision), upserts, removes)
        } else {
            (None, poses.values().copied().collect(), Vec::new())
        };
        if base_revision.is_some() && upserts.is_empty() && removes.is_empty() {
            return Ok((None, work));
        }
        let revision = self.next_revision;
        let next_revision = revision.checked_add(1).ok_or(EntityError::Capacity)?;
        let frame = EntityFrame {
            revision,
            base: base_revision,
            tick,
            upserts,
            removes,
        };
        frame.validate().map_err(|_| EntityError::InvalidEncoding)?;
        work.upserts = frame.upserts.len() as u64;
        work.removes = frame.removes.len() as u64;
        work.full = frame.base.is_none();
        self.visible = poses.keys().copied().collect();
        if self.images.len() == self.versions {
            self.images.pop_front();
        }
        self.images.push_back(Image { revision, poses });
        self.next_revision = next_revision;
        self.force_full = false;
        Ok((Some(frame), work))
    }
}
fn charge(query: QueryWork, budget: &mut QueryBudget, work: &mut Work) -> Result<(), EntityError> {
    budget.buckets = budget.buckets.saturating_sub(query.buckets);
    budget.probes = budget.probes.saturating_sub(query.probes);
    budget.candidates = budget.candidates.saturating_sub(query.candidates);
    work.buckets += query.buckets;
    work.probes += query.probes;
    work.candidates += query.candidates;
    if query.overflow {
        return Err(EntityError::Capacity);
    }
    Ok(())
}
fn public_pose(
    id: EntityId,
    space: SpaceId,
    position: [f64; 3],
    instance: Instance,
    count: u16,
    damage: u16,
) -> Result<EntityPose, EntityError> {
    let pose = EntityPose {
        id: id.0,
        space,
        position,
        body_yaw: instance.body_yaw,
        head_yaw: instance.head_yaw,
        pitch: instance.pitch,
        limb_phase: instance.limb_phase,
        limb_amount: instance.limb_amount,
        death_roll: instance.death_roll,
        scale: instance.scale,
        extra: instance.reserved,
        kind: instance.kind as u8,
        flags: instance.flags as u16,
        color: instance.color,
        count,
        damage,
    };
    pose.validate().map_err(|_| EntityError::InvalidEncoding)?;
    Ok(pose)
}
