//! Allocation-free movement view over an already validated PredictionState.
//! Audio needs no Player/collision scratch owner and cannot inspect permissions.
use super::{PREDICTION_VERSION, PredictionState, Reader, game_mode, valid_position};
use crate::{GameMode, Player};
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioMotion {
    pub position: [f64; 3],
    pub previous: [f64; 3],
    pub health: f32,
    pub on_ground: bool,
    pub flying: bool,
    pub mode: GameMode,
    pub sneaking: bool,
    pub crouching: bool,
    pub support: Option<[i32; 3]>,
}
impl AudioMotion {
    pub fn from_player(player: &Player) -> Self {
        Self {
            position: player.pos,
            previous: player.prev,
            health: player.health,
            on_ground: player.on_ground,
            flying: player.flying,
            mode: player.mode,
            sneaking: player.sneak_in,
            crouching: player.crouch,
            support: player.support,
        }
    }
}
impl PredictionState {
    /// The owning state was fully validated by capture/decode. Version/header
    /// guards keep this small projection explicit when the wire format changes.
    pub fn audio_motion(&self) -> Result<AudioMotion, &'static str> {
        let mut read = Reader {
            bytes: self.as_bytes(),
            offset: 0,
        };
        if read.array::<4>()? != *b"PRDT" || u16::from_le_bytes(read.array()?) != PREDICTION_VERSION
        {
            return Err("unsupported audio prediction view");
        }
        let position = [read.f64()?, read.f64()?, read.f64()?];
        let _ = read.array::<32>()?; // velocity, yaw and pitch.
        let health = read.f32()?;
        let on_ground = read.bool()?;
        let flying = read.bool()?;
        let _ = read.bool()?;
        let mode = game_mode(read.array::<1>()?[0])?;
        let previous = [read.f64()?, read.f64()?, read.f64()?];
        let _ = read.array::<56>()?; // fourteen public animation/motion floats.
        let _ = read.array::<3>()?; // horizontal collision, soft, sprinting.
        let sneaking = read.bool()?;
        let crouching = read.bool()?;
        let _ = read.array::<40>()?; // fluid/swing flags, counters, fluid heights, tick.
        let support = if read.bool()? {
            Some([
                i32::from_le_bytes(read.array()?),
                i32::from_le_bytes(read.array()?),
                i32::from_le_bytes(read.array()?),
            ])
        } else {
            None
        };
        if !valid_position(position) || !valid_position(previous) || !(0.0..=20.0).contains(&health)
        {
            return Err("invalid audio motion projection");
        }
        Ok(AudioMotion {
            position,
            previous,
            health,
            on_ground,
            flying,
            mode,
            sneaking,
            crouching,
            support,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_projection_matches_real_player_fields_for_all_modes_and_optional_support() {
        for mode in [GameMode::Survival, GameMode::Creative, GameMode::Spectator] {
            for support in [None, Some([-7, 63, 12])] {
                let mut player = Player::new([1.25, 64.0, -7.5]);
                player.prev = [1.0, 64.0, -7.75];
                player.mode = mode;
                player.support = support;
                player.sneak_in = true;
                player.crouch = true;
                player.health = 7.5;
                let state = PredictionState::capture(&player).unwrap();
                assert_eq!(
                    state.audio_motion().unwrap(),
                    AudioMotion::from_player(&player)
                );
            }
        }
    }
}
