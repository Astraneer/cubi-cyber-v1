//! Public player poses use the same retained body animation as the local player.
//! No actor physics, permissions, block generation or CPU model vertices run here.
use core_types::{SpaceId, Tick};
use net::{PlayerId, PlayerState};
use rules::entities::mob::Instance;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Capacity,
    Invalid,
    Stale,
}
struct Row {
    id: PlayerId,
    previous: PlayerState,
    current: PlayerState,
    previous_tick: Tick,
    current_tick: Tick,
    body: sim::SelfBody,
    previous_pose: [f32; 4],
    pose: [f32; 4],
}
pub struct RemoteHumans {
    rows: Vec<Row>,
    maximum: usize,
    tick: Tick,
}
impl RemoteHumans {
    pub fn new(maximum: usize) -> Result<Self, Error> {
        if maximum == 0 || maximum > 64 {
            return Err(Error::Capacity);
        }
        Ok(Self {
            rows: Vec::with_capacity(maximum),
            maximum,
            tick: Tick(0),
        })
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    /// `render_tick` is the same delayed public server time used for mobs/items.
    /// The supplied output is a preallocated suffix of the combined instance bank.
    #[allow(clippy::too_many_arguments)]
    pub fn write<'a>(
        &mut self,
        tick: Tick,
        render_tick: f64,
        players: impl Iterator<Item = &'a PlayerState>,
        own: PlayerId,
        space: SpaceId,
        origin: [i64; 3],
        output: &mut [Instance],
    ) -> Result<usize, Error> {
        if tick < self.tick {
            return Err(Error::Stale);
        }
        if !render_tick.is_finite() {
            return Err(Error::Invalid);
        }
        let mut source = [None; 64];
        let mut count = 0;
        for player in players {
            if player.identity == own || player.space != space || player.mode == 3 {
                continue;
            }
            if count == self.maximum || count == output.len() {
                return Err(Error::Capacity);
            }
            if player.mode > 3
                || !player.yaw.is_finite()
                || !player.pitch.is_finite()
                || !player.health.is_finite()
                || player
                    .position
                    .iter()
                    .chain(player.velocity.iter())
                    .any(|value| !value.is_finite() || value.abs() > f64::from(i32::MAX))
                || source[..count]
                    .iter()
                    .flatten()
                    .any(|old: &&PlayerState| old.identity == player.identity)
            {
                return Err(Error::Invalid);
            }
            source[count] = Some(player);
            count += 1;
        }
        self.rows.retain(|row| {
            source[..count]
                .iter()
                .flatten()
                .any(|player| player.identity == row.id)
        });
        for player in source[..count].iter().flatten().copied() {
            if let Some(row) = self.rows.iter_mut().find(|row| row.id == player.identity) {
                if tick > row.current_tick {
                    row.previous = row.current;
                    row.previous_tick = row.current_tick;
                    row.current = *player;
                    row.current_tick = tick;
                    row.previous_pose = row.pose;
                    let delta: [f64; 3] = std::array::from_fn(|axis| {
                        row.current.position[axis] - row.previous.position[axis]
                    });
                    let teleport = row.previous.space != row.current.space
                        || delta.iter().map(|v| v * v).sum::<f64>() > 256.;
                    let steps = (tick.0 - row.previous_tick.0).clamp(1, 64);
                    if teleport {
                        row.previous = row.current;
                        row.previous_tick = tick;
                        row.body.snap(player.yaw);
                    }
                    for step in 0..steps {
                        let from = if teleport {
                            player.position
                        } else {
                            std::array::from_fn(|axis| {
                                row.previous.position[axis]
                                    + delta[axis] * step as f64 / steps as f64
                            })
                        };
                        let to = if teleport {
                            player.position
                        } else {
                            std::array::from_fn(|axis| {
                                row.previous.position[axis]
                                    + delta[axis] * (step + 1) as f64 / steps as f64
                            })
                        };
                        row.body
                            .advance_motion(from, to, player.yaw, false)
                            .map_err(|_| Error::Invalid)?;
                    }
                    row.pose = row.body.pose(1.);
                    if teleport {
                        row.previous_pose = row.pose;
                    }
                }
            } else {
                let mut body = sim::SelfBody::default();
                body.snap(player.yaw);
                let pose = body.pose(1.);
                self.rows.push(Row {
                    id: player.identity,
                    previous: *player,
                    current: *player,
                    previous_tick: tick,
                    current_tick: tick,
                    body,
                    previous_pose: pose,
                    pose,
                });
            }
        }
        for (index, player) in source[..count].iter().flatten().enumerate() {
            let row = self
                .rows
                .iter()
                .find(|row| row.id == player.identity)
                .expect("retained public player");
            let a = if row.current_tick == row.previous_tick {
                1.
            } else {
                ((render_tick - row.previous_tick.0 as f64)
                    / (row.current_tick.0 - row.previous_tick.0) as f64)
                    .clamp(0., 1.)
            };
            let pose: [f32; 4] = std::array::from_fn(|axis| {
                let delta = if axis == 0 {
                    sim::wrap_degrees(row.pose[axis] - row.previous_pose[axis])
                } else {
                    row.pose[axis] - row.previous_pose[axis]
                };
                row.previous_pose[axis] + delta * a as f32
            });
            let id = player
                .identity
                .0
                .iter()
                .fold(0xcbf29ce484222325_u64, |hash, byte| {
                    (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
                });
            output[index] = Instance {
                position: std::array::from_fn(|axis| {
                    ((row.previous.position[axis]
                        + (row.current.position[axis] - row.previous.position[axis]) * a)
                        - origin[axis] as f64) as f32
                }),
                body_yaw: pose[0],
                head_yaw: row.previous.yaw
                    + sim::wrap_degrees(row.current.yaw - row.previous.yaw) * a as f32,
                pitch: (row.previous.pitch + (row.current.pitch - row.previous.pitch) * a as f32)
                    .clamp(-90., 90.),
                limb_phase: pose[1],
                limb_amount: pose[2],
                kind: 5,
                flags: 0,
                color: pose[3].to_bits(),
                id_low: id as u32,
                id_high: (id >> 32) as u32,
                death_roll: if player.health <= 0. { 90. } else { 0. },
                scale: 1.,
                reserved: 0.,
            };
        }
        self.tick = tick;
        Ok(count)
    }
}
