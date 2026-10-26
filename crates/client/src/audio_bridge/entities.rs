//! Audible presentation of confirmed public poses. No AI, inventory, health or
//! private event stream is queried. A first image and repeated revisions are
//! silent; entering range cannot replay an old hurt/death transition.
use super::BlockCue;
use crate::network::Connection;
use net::{PlayerId, entities::EntityPose};

const MAX_CUES: usize = 16;
const RANGE_SQUARED: f64 = 32. * 32.;
#[derive(Clone, Copy)]
struct Row {
    id: u64,
    kind: u8,
    flags: u16,
    phase: f32,
}
pub struct EntitySounds {
    session: Option<(u64, PlayerId)>,
    revision: Option<u64>,
    rows: Vec<Row>,
    next: Vec<Row>,
    pub emitted: u64,
    pub dropped: u64,
}
impl Default for EntitySounds {
    fn default() -> Self {
        Self {
            session: None,
            revision: None,
            rows: Vec::with_capacity(net::entities::MAX_ENTITIES),
            next: Vec::with_capacity(net::entities::MAX_ENTITIES),
            emitted: 0,
            dropped: 0,
        }
    }
}
impl EntitySounds {
    pub fn clear(&mut self) {
        self.session = None;
        self.revision = None;
        self.rows.clear();
        self.next.clear();
    }
    pub fn visit(
        &mut self,
        network: &Connection,
        listener: [f64; 3],
        mut emit: impl FnMut(BlockCue),
    ) -> usize {
        if listener.iter().any(|value| !value.is_finite()) {
            return 0;
        }
        let Some(welcome) = network.welcome() else {
            self.clear();
            return 0;
        };
        let session = Some((welcome.session, welcome.identity));
        if self.session != session {
            self.clear();
            self.session = session;
        }
        let replica = network.entity_replica();
        if replica.revision() == self.revision {
            return 0;
        }
        self.revision = replica.revision();
        self.next.clear();
        let mut count = 0;
        for pose in replica.poses() {
            if pose.space != welcome.player.space
                || pose.kind > 4
                || pose
                    .position
                    .iter()
                    .zip(listener)
                    .map(|(&a, b)| (a - b) * (a - b))
                    .sum::<f64>()
                    > RANGE_SQUARED
            {
                continue;
            }
            if let Ok(index) = self.rows.binary_search_by_key(&pose.id, |row| row.id) {
                let old = self.rows[index];
                if old.kind == pose.kind {
                    for event in transitions(old, &pose).into_iter().flatten() {
                        if count == MAX_CUES {
                            self.dropped = self.dropped.saturating_add(1);
                            continue;
                        }
                        emit(BlockCue {
                            event,
                            gain: if event.ends_with(".step") { 0.15 } else { 1. },
                            position: pose.position,
                            emitter: audio::EmitterId((pose.id << 2) | 2),
                            seed: welcome.session ^ pose.id ^ self.revision.unwrap_or(0),
                        });
                        count += 1;
                    }
                }
            }
            self.next.push(Row {
                id: pose.id,
                kind: pose.kind,
                flags: pose.flags,
                phase: pose.limb_phase,
            });
        }
        std::mem::swap(&mut self.rows, &mut self.next);
        self.emitted = self.emitted.saturating_add(count as u64);
        count
    }
}
fn transitions(old: Row, pose: &EntityPose) -> [Option<&'static str>; 3] {
    let died = old.flags & 4 == 0 && pose.flags & 4 != 0;
    let hurt = old.flags & 16 == 0 && pose.flags & 16 != 0 && !died;
    let sheared = pose.kind == 2 && old.flags & 2 == 0 && pose.flags & 2 != 0;
    // The render owner's actual unwrapped limb phase advances with confirmed
    // motion. One half-period is one foot contact; standing phase is unchanged.
    let frequency = 0.6662 * if pose.flags & 1 != 0 { 3. } else { 1. };
    let contact = pose.limb_phase > old.phase
        && old.flags & 1 == pose.flags & 1
        && libm::floorf(pose.limb_phase * frequency / std::f32::consts::PI)
            > libm::floorf(old.phase * frequency / std::f32::consts::PI);
    let step = pose.flags & 4 == 0 && pose.flags & 64 != 0 && pose.limb_amount > 0.05 && contact;
    let effect = if died {
        match pose.kind {
            0 => Some("entity.pig.death"),
            4 => Some("entity.zombie.death"),
            _ => None,
        }
    } else if hurt {
        match pose.kind {
            0 => Some("entity.pig.hurt"),
            1 => Some("entity.cow.hurt"),
            3 => Some("entity.chicken.hurt"),
            4 => Some("entity.zombie.hurt"),
            _ => None,
        }
    } else {
        None
    };
    let foot = if step {
        Some(match pose.kind {
            0 => "entity.pig.step",
            1 => "entity.cow.step",
            2 => "entity.sheep.step",
            3 => "entity.chicken.step",
            _ => "entity.zombie.step",
        })
    } else {
        None
    };
    [effect, sheared.then_some("entity.sheep.shear"), foot]
}
