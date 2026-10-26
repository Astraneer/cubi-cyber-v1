//! Read-only fixed-tick audio inputs. Queries never generate/load world cells.
mod actions;
mod cues;
mod entities;
mod machines;
pub use actions::{BlockCue, ConfirmedBlockSounds};
use audio::{
    Listener,
    echo::Cavity,
    events::{StepBody, Surface},
};
use core_types::Cell;
pub use cues::CausalSounds;
pub use entities::EntitySounds;
pub use machines::{MachineCommand, MachineSoundWork, MachineSounds};
use rules::blocks::Snd;
use sim::{Player, QueryWorld};
use world::CellRead;
pub fn surface(cell: Cell) -> Surface {
    match rules::block(cell.id())
        .map(|b| b.sound)
        .unwrap_or(Snd::None)
    {
        Snd::None => Surface::None,
        Snd::Stone => Surface::Stone,
        Snd::Wood => Surface::Wood,
        Snd::Gravel => Surface::Gravel,
        Snd::Grass => Surface::Grass,
        Snd::Sand => Surface::Sand,
        Snd::Snow => Surface::Snow,
        Snd::Wool => Surface::Wool,
        Snd::Glass => Surface::Glass,
        Snd::Metal => Surface::Metal,
        Snd::Ladder => Surface::Ladder,
    }
}
/// Uses the physics owner's actual support, matching edge/half-block footsteps.
/// Unknown support produces silence rather than reading/generating another page.
pub fn step_body(player: &Player, world: &impl QueryWorld) -> StepBody {
    step_motion(&sim::prediction::AudioMotion::from_player(player), world)
}
pub fn step_motion(player: &sim::prediction::AudioMotion, world: &impl QueryWorld) -> StepBody {
    let feet = player.position.map(|v| libm::floor(v) as i32);
    let mut stepping = feet;
    stepping[1] = libm::floor(player.position[1] - 1e-5) as i32;
    let support = player.support.unwrap_or([
        feet[0],
        libm::floor(player.position[1] - 0.2) as i32,
        feet[2],
    ]);
    let support_cell = world.cell(support);
    let mut above = support;
    above[1] = above[1].saturating_add(1);
    let climbing =
        matches!(world.cell(feet), CellRead::Known(cell) if cell.id() == rules::ids::LADDER);
    StepBody {
        moved: std::array::from_fn(|i| player.position[i] - player.previous[i]),
        grounded: player.on_ground,
        climbing,
        silent: player.flying
            || player.mode == sim::GameMode::Spectator
            || (player.sneaking && player.on_ground)
            || player.health <= 0.0,
        crouching: player.crouching,
        stepping_air: !matches!(world.cell(stepping), CellRead::Known(cell) if cell.id() != rules::ids::AIR),
        support: match support_cell {
            CellRead::Known(cell) => surface(cell),
            CellRead::Unknown => Surface::None,
        },
        snow_above_support: matches!(world.cell(above), CellRead::Known(cell) if cell.id() == rules::ids::SNOW_LAYER),
    }
}
pub fn listener(player: &Player, eye: [f64; 3]) -> Listener {
    let yaw = f64::from(player.yaw).to_radians();
    Listener {
        position: eye,
        right: [-libm::cos(yaw) as f32, 0.0, -libm::sin(yaw) as f32],
    }
}
/// At most 6×16 existing-cell reads. Call only when cave acoustics is enabled,
/// outside the audio callback; unknown/open rays never become imaginary walls.
pub fn probe_cavity(origin: [f64; 3], world: &impl QueryWorld) -> Cavity {
    let start = origin.map(|v| libm::floor(v) as i32);
    let mut walls = [None; 6];
    for (face, wall) in walls.iter_mut().enumerate() {
        let axis = face / 2;
        let direction = if face % 2 == 0 { -1 } else { 1 };
        for step in 1..=16 {
            let mut at = start;
            let Some(coord) = at[axis].checked_add(direction * step) else {
                break;
            };
            at[axis] = coord;
            match world.cell(at) {
                CellRead::Unknown => break,
                CellRead::Known(cell) if rules::properties(cell).opaque => {
                    let boundary = f64::from(coord) + if direction < 0 { 1.0 } else { 0.0 };
                    *wall = Some((boundary - origin[axis]).abs().max(0.1) as f32);
                    break;
                }
                _ => {}
            }
        }
    }
    Cavity {
        wall_distances: walls,
        absorption: 0.3,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockSound {
    Break,
    Place,
    Hit,
}
/// Presentation cursor over the real fixed-tick prediction history. A slow
/// render frame can visit up to 64 distinct ticks without losing their distance.
#[derive(Default)]
pub struct PredictionSounds {
    session: Option<(u64, net::PlayerId)>,
    tick: Option<u64>,
    health: Option<u32>,
    position: Option<[f64; 3]>,
}
impl PredictionSounds {
    pub fn bind(&mut self, network: &crate::network::Connection) -> Option<audio::EmitterId> {
        let next = network
            .welcome()
            .map(|welcome| (welcome.session, welcome.identity));
        if self.session == next {
            return None;
        }
        let retired = self.session.map(|(_, identity)| emitter(identity));
        self.session = next;
        self.tick = None;
        self.health = None;
        self.position = None;
        retired
    }
    pub fn emitter(&self) -> Option<audio::EmitterId> {
        self.session.map(|(_, id)| emitter(id))
    }
    pub fn visit(
        &mut self,
        network: &crate::network::Connection,
        world: &impl QueryWorld,
        mut visitor: impl FnMut(core_types::Tick, StepBody, audio::EmitterId, [f64; 3]),
    ) -> Result<usize, &'static str> {
        let Some(id) = self.emitter() else {
            return Ok(0);
        };
        let end = network.predicted_tick().0;
        let start = match self.tick {
            Some(old) if old >= end => return Ok(0),
            Some(old) => old.saturating_add(1).max(end.saturating_sub(63)),
            None => end,
        };
        let mut count = 0;
        for (tick, state) in network
            .audio_history(core_types::Tick(start), core_types::Tick(end))
            .map_err(|_| "audio history bounds")?
        {
            let player = state.audio_motion()?;
            let mut body = step_motion(&player, world);
            // Waiting for Unknown does not run Motion::tick, so its public
            // `prev` may still describe an earlier move. Never replay it.
            if self.position == Some(player.position) {
                body.moved = [0.0; 3];
            }
            self.position = Some(player.position);
            visitor(tick, body, id, player.position);
            self.tick = Some(tick.0);
            count += 1;
        }
        Ok(count)
    }
    pub fn health_event(
        &mut self,
        network: &crate::network::Connection,
    ) -> Option<(&'static str, audio::EmitterId, [f64; 3])> {
        let player = network.player()?;
        let old = self.health.replace(player.health.to_bits())?;
        if player.health >= f32::from_bits(old) {
            return None;
        }
        Some((
            if player.health <= 0.0 {
                "player.death"
            } else {
                "player.hurt"
            },
            self.emitter()?,
            player.pos,
        ))
    }
}
fn emitter(identity: net::PlayerId) -> audio::EmitterId {
    let first = u64::from_le_bytes(identity.0[..8].try_into().unwrap());
    let second = u64::from_le_bytes(identity.0[8..].try_into().unwrap());
    // Reserve low bits 3 for local UI sounds; snow's second support cue uses +1.
    audio::EmitterId(((first ^ second.rotate_left(17)) & !3) | 1)
}
pub fn block_event(cell: Cell, action: BlockSound) -> Option<(&'static str, f32)> {
    let surface = surface(cell);
    if surface == Surface::None {
        return None;
    }
    if action == BlockSound::Hit {
        return surface.step_event().map(|event| (event, 0.25));
    }
    if action == BlockSound::Place {
        return surface.step_event().map(|event| (event, 1.0));
    }
    Some((
        match surface {
            Surface::Glass => "block.glass.break",
            Surface::Wood | Surface::Ladder => "block.wood.dig",
            Surface::Gravel => "block.gravel.dig",
            Surface::Grass => "block.grass.dig",
            Surface::Sand => "block.sand.dig",
            Surface::Snow => "block.snow.dig",
            Surface::Wool => "block.cloth.dig",
            _ => "block.stone.dig",
        },
        1.0,
    ))
}
