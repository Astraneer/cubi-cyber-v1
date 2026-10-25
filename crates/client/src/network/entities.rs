//! Public actor replica and preallocated constant-size render output. Transport
//! and the connection own ACK delivery; rendering never touches private owners.
use core_types::{SpaceId, Tick};
use net::entities::{EntityFrame, EntityPose, EntityRequest, MAX_ENTITIES, resulting_ids};
use rules::entities::mob::Instance;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Apply {
    Applied(EntityRequest),
    Stale(EntityRequest),
    MissingBase(EntityRequest),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplicaError {
    Capacity,
    Invalid,
    Time,
    Kind,
}
#[derive(Clone, Copy)]
struct Samples {
    previous: EntityPose,
    current: EntityPose,
    previous_tick: Tick,
    current_tick: Tick,
}
pub struct EntityReplica {
    rows: BTreeMap<u64, Samples>,
    revision: Option<u64>,
    tick: Tick,
    maximum: usize,
}
impl EntityReplica {
    pub fn new(maximum: usize) -> Result<Self, ReplicaError> {
        if maximum == 0 || maximum > MAX_ENTITIES {
            return Err(ReplicaError::Capacity);
        }
        Ok(Self {
            rows: BTreeMap::new(),
            revision: None,
            tick: Tick(0),
            maximum,
        })
    }
    pub const fn revision(&self) -> Option<u64> {
        self.revision
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn get(&self, id: u64) -> Option<EntityPose> {
        self.rows.get(&id).map(|sample| sample.current)
    }
    pub fn poses(&self) -> impl Iterator<Item = EntityPose> + '_ {
        self.rows.values().map(|sample| sample.current)
    }
    /// Entire lifecycle preflight precedes mutation. A missing base requests a
    /// full image while preserving the previous complete visible scene.
    pub fn apply(&mut self, frame: EntityFrame) -> Result<Apply, ReplicaError> {
        frame.validate().map_err(|_| ReplicaError::Invalid)?;
        if self
            .revision
            .is_some_and(|revision| frame.revision <= revision)
        {
            return Ok(Apply::Stale(EntityRequest::Ack {
                revision: self.revision.expect("known revision"),
            }));
        }
        if frame.base.is_some() && frame.base != self.revision {
            return Ok(Apply::MissingBase(EntityRequest::Full {
                have: self.revision,
            }));
        }
        if self.revision.is_some() && frame.tick < self.tick {
            return Err(ReplicaError::Time);
        }
        resulting_ids(&frame, self.rows.keys().copied(), self.maximum)
            .map_err(|_| ReplicaError::Capacity)?;
        if frame.base.is_some()
            && frame.upserts.iter().any(|pose| {
                self.rows
                    .get(&pose.id)
                    .is_some_and(|old| old.current.kind != pose.kind)
            })
        {
            return Err(ReplicaError::Kind);
        }
        let mut next = if frame.base.is_none() {
            BTreeMap::new()
        } else {
            self.rows.clone()
        };
        for id in &frame.removes {
            next.remove(id);
        }
        for pose in frame.upserts {
            let old = self.rows.get(&pose.id).filter(|old| {
                old.current.kind == pose.kind
                    && old.current.space == pose.space
                    && old.current_tick < frame.tick
            });
            let sample = if let Some(old) = old {
                Samples {
                    previous: old.current,
                    current: pose,
                    previous_tick: old.current_tick,
                    current_tick: frame.tick,
                }
            } else {
                Samples {
                    previous: pose,
                    current: pose,
                    previous_tick: frame.tick,
                    current_tick: frame.tick,
                }
            };
            next.insert(pose.id, sample);
        }
        self.rows = next;
        self.revision = Some(frame.revision);
        self.tick = frame.tick;
        Ok(Apply::Applied(EntityRequest::Ack {
            revision: frame.revision,
        }))
    }
    /// `time` is a server tick coordinate, normally delayed for interpolation.
    /// Positions subtract the integer camera origin before the f32 conversion.
    /// The caller retains `out`; this path allocates no objects or geometry.
    pub fn instances(
        &self,
        space: SpaceId,
        time: f64,
        origin: [i64; 3],
        out: &mut [Instance],
    ) -> Result<usize, ReplicaError> {
        if !time.is_finite() {
            return Err(ReplicaError::Time);
        }
        let count = self
            .rows
            .values()
            .filter(|sample| sample.current.space == space)
            .count();
        if count > out.len() {
            return Err(ReplicaError::Capacity);
        }
        for (sample, instance) in self
            .rows
            .values()
            .filter(|sample| sample.current.space == space)
            .zip(out.iter_mut())
        {
            let (a, b) = (sample.previous, sample.current);
            let alpha = if sample.previous_tick == sample.current_tick {
                1.0
            } else {
                ((time - sample.previous_tick.0 as f64)
                    / (sample.current_tick.0 - sample.previous_tick.0) as f64)
                    .clamp(0.0, 1.0)
            };
            let mix = |x: f32, y: f32| x + (y - x) * alpha as f32;
            let angle =
                |x: f32, y: f32| x + ((y - x + 180.0).rem_euclid(360.0) - 180.0) * alpha as f32;
            *instance = Instance {
                position: std::array::from_fn(|axis| {
                    (a.position[axis] - origin[axis] as f64
                        + (b.position[axis] - a.position[axis]) * alpha) as f32
                }),
                body_yaw: angle(a.body_yaw, b.body_yaw),
                head_yaw: angle(a.head_yaw, b.head_yaw),
                pitch: mix(a.pitch, b.pitch),
                limb_phase: mix(a.limb_phase, b.limb_phase),
                limb_amount: mix(a.limb_amount, b.limb_amount),
                kind: u32::from(b.kind),
                flags: u32::from(b.flags),
                color: if b.kind == 3 {
                    mix(f32::from_bits(a.color), f32::from_bits(b.color)).to_bits()
                } else {
                    b.color
                },
                id_low: b.id as u32,
                id_high: (b.id >> 32) as u32,
                death_roll: mix(a.death_roll, b.death_roll),
                scale: b.scale,
                reserved: mix(a.extra, b.extra),
            };
        }
        Ok(count)
    }
}
