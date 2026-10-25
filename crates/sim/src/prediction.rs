//! Exact public simulation rollback state. Contains only movement/health data;
//! private grants, credentials, inventories and generation entropy are excluded.
//! A server never accepts this state from a client: only authoritative messages
//! may be installed by client prediction.
use crate::{GameMode, Player};

pub const PREDICTION_VERSION: u16 = 1;
pub const MAX_PREDICTION_BYTES: usize = 384;
const POSITION_LIMIT: f64 = 30_000_000.0;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PredictionState {
    bytes: [u8; MAX_PREDICTION_BYTES],
    len: u16,
}
impl PredictionState {
    pub fn capture(player: &Player) -> Result<Self, &'static str> {
        let mut out = Writer {
            bytes: [0; MAX_PREDICTION_BYTES],
            len: 0,
        };
        out.put(b"PRDT");
        out.put(&PREDICTION_VERSION.to_le_bytes());
        for value in player.pos.into_iter().chain(player.vel) {
            out.f64(value);
        }
        for value in [player.yaw, player.pitch, player.health] {
            out.f32(value);
        }
        for value in [player.on_ground, player.flying, player.noclip] {
            out.bool(value);
        }
        out.put(&[player.mode as u8]);
        for value in player.prev {
            out.f64(value);
        }
        for value in [
            player.fall,
            player.fwd_in,
            player.side,
            player.fwd,
            player.hspeed,
            player.prev_hspeed,
            player.stride,
            player.prev_stride,
            player.cam_y,
            player.last_cam_y,
            player.fov,
            player.last_fov,
            player.swing,
            player.last_swing,
        ] {
            out.f32(value);
        }
        for value in [
            player.h_coll,
            player.soft,
            player.sprinting,
            player.sneak_in,
            player.crouch,
            player.in_water,
            player.in_lava,
            player.eye_water,
            player.swinging,
        ] {
            out.bool(value);
        }
        for value in [player.jump_cd, player.dtap, player.swing_ticks] {
            out.put(&value.to_le_bytes());
        }
        out.f64(player.fluid_h);
        out.f64(player.lava_h);
        out.put(&player.ticks.to_le_bytes());
        out.bool(player.support.is_some());
        if let Some(position) = player.support {
            for axis in position {
                out.put(&axis.to_le_bytes());
            }
        }
        out.bool(player.landed.is_some());
        if let Some((tick, distance)) = player.landed {
            out.put(&tick.to_le_bytes());
            out.f32(distance);
        }

        let state = Self {
            bytes: out.bytes,
            len: out.len as u16,
        };
        state.to_player()?;
        Ok(state)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_PREDICTION_BYTES {
            return Err("prediction state exceeds bound");
        }
        let mut state = Self {
            bytes: [0; MAX_PREDICTION_BYTES],
            len: bytes.len() as u16,
        };
        state.bytes[..bytes.len()].copy_from_slice(bytes);
        state.to_player()?;
        Ok(state)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
    pub fn hash(&self) -> u64 {
        self.as_bytes().iter().fold(0xcbf29ce484222325, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        })
    }
    /// Install a validated state received from the authority. This method is not
    /// a server-side permission or movement request API.
    pub fn apply_authoritative(&self, player: &mut Player) -> Result<(), &'static str> {
        *player = self.to_player()?;
        Ok(())
    }
    pub fn to_player(&self) -> Result<Player, &'static str> {
        let mut input = Reader {
            bytes: self.as_bytes(),
            offset: 0,
        };
        if input.array::<4>()? != *b"PRDT" {
            return Err("invalid prediction magic");
        }
        if u16::from_le_bytes(input.array()?) != PREDICTION_VERSION {
            return Err("unsupported prediction version");
        }
        let position = [input.f64()?, input.f64()?, input.f64()?];
        let mut player = Player::new(position);
        player.vel = [input.f64()?, input.f64()?, input.f64()?];
        player.yaw = input.f32()?;
        player.pitch = input.f32()?;
        player.health = input.f32()?;
        player.on_ground = input.bool()?;
        player.flying = input.bool()?;
        player.noclip = input.bool()?;
        player.mode = game_mode(input.array::<1>()?[0])?;
        player.prev = [input.f64()?, input.f64()?, input.f64()?];
        if !valid_position(player.prev) {
            return Err("invalid previous player position");
        }
        player.fall = input.f32()?;
        player.fwd_in = input.f32()?;
        player.side = input.f32()?;
        player.fwd = input.f32()?;
        player.hspeed = input.f32()?;
        player.prev_hspeed = input.f32()?;
        player.stride = input.f32()?;
        player.prev_stride = input.f32()?;
        player.cam_y = input.f32()?;
        player.last_cam_y = input.f32()?;
        player.fov = input.f32()?;
        player.last_fov = input.f32()?;
        player.swing = input.f32()?;
        player.last_swing = input.f32()?;
        player.h_coll = input.bool()?;
        player.soft = input.bool()?;
        player.sprinting = input.bool()?;
        player.sneak_in = input.bool()?;
        player.crouch = input.bool()?;
        player.in_water = input.bool()?;
        player.in_lava = input.bool()?;
        player.eye_water = input.bool()?;
        player.swinging = input.bool()?;
        player.jump_cd = i32::from_le_bytes(input.array()?);
        player.dtap = i32::from_le_bytes(input.array()?);
        player.swing_ticks = i32::from_le_bytes(input.array()?);
        player.fluid_h = input.f64()?;
        player.lava_h = input.f64()?;
        player.ticks = u64::from_le_bytes(input.array()?);
        player.support = if input.bool()? {
            let position = [
                i32::from_le_bytes(input.array()?),
                i32::from_le_bytes(input.array()?),
                i32::from_le_bytes(input.array()?),
            ];
            if position
                .iter()
                .any(|axis| axis.unsigned_abs() >= POSITION_LIMIT as u32)
            {
                return Err("invalid player support position");
            }
            Some(position)
        } else {
            None
        };
        player.landed = if input.bool()? {
            let tick = u64::from_le_bytes(input.array()?);
            let distance = input.f32()?;
            if tick > player.ticks || distance < 0.0 {
                return Err("invalid player landing continuation");
            }
            Some((tick, distance))
        } else {
            None
        };
        if input.offset != input.bytes.len() {
            return Err("trailing player continuation data");
        }
        if player.fall < 0.0
            || player.fluid_h < 0.0
            || player.lava_h < 0.0
            || player.ticks == u64::MAX
            || !(0..=10).contains(&player.jump_cd)
            || !(0..=7).contains(&player.dtap)
            || !(-1..=5).contains(&player.swing_ticks)
        {
            return Err("invalid player continuation bounds");
        }
        if !valid_position(player.pos)
            || player.vel.iter().any(|v| v.abs() > 64.0)
            || player.pitch.abs() > 90.0
            || !(0.0..=20.0).contains(&player.health)
            || player.noclip && !player.flying
        {
            return Err("invalid public prediction bounds");
        }
        player.debug_permission = player.noclip && player.mode != GameMode::Spectator;
        player.self_body.snap(player.yaw);
        Ok(player)
    }
}

fn game_mode(mode: u8) -> Result<GameMode, &'static str> {
    match mode {
        0 => Ok(GameMode::Survival),
        1 => Ok(GameMode::Creative),
        3 => Ok(GameMode::Spectator),
        _ => Err("invalid player game mode"),
    }
}
fn valid_position(position: [f64; 3]) -> bool {
    position
        .iter()
        .all(|axis| axis.is_finite() && axis.abs() < POSITION_LIMIT)
}

struct Writer {
    bytes: [u8; MAX_PREDICTION_BYTES],
    len: usize,
}
impl Writer {
    fn put(&mut self, bytes: &[u8]) {
        let end = self.len + bytes.len();
        self.bytes[self.len..end].copy_from_slice(bytes);
        self.len = end;
    }
    fn f32(&mut self, v: f32) {
        self.put(&v.to_bits().to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.put(&v.to_bits().to_le_bytes());
    }
    fn bool(&mut self, v: bool) {
        self.put(&[u8::from(v)]);
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl Reader<'_> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], &'static str> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or("player continuation offset")?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or("truncated player continuation")?;
        self.offset = end;
        bytes
            .try_into()
            .map_err(|_| "player continuation fixed length")
    }
    fn f32(&mut self) -> Result<f32, &'static str> {
        let value = f32::from_bits(u32::from_le_bytes(self.array()?));
        if value.is_finite() {
            Ok(value)
        } else {
            Err("nonfinite player continuation")
        }
    }
    fn f64(&mut self) -> Result<f64, &'static str> {
        let value = f64::from_bits(u64::from_le_bytes(self.array()?));
        if value.is_finite() {
            Ok(value)
        } else {
            Err("nonfinite player continuation")
        }
    }
    fn bool(&mut self) -> Result<bool, &'static str> {
        match self.array::<1>()?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("invalid player continuation boolean"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Keys;
    use core_types::Cell;
    use world::CellRead;

    fn query(p: [i32; 3]) -> CellRead {
        let id = if p[1] < 0 || p[2] == 5 && (0..4).contains(&p[1]) {
            rules::ids::STONE
        } else if (3..7).contains(&p[0]) && (0..4).contains(&p[1]) {
            rules::ids::WATER
        } else {
            rules::ids::AIR
        };
        CellRead::Known(Cell::from_parts(id, 0))
    }
    #[test]
    fn exact_rollback_preserves_next_motion_tick_across_ground_flight_crouch_and_fluids() {
        let mut player = Player::new([0.5, 4.0, 0.5]);
        for tick in 0..320 {
            if tick == 90 {
                player.teleport([4.5, 1.0, 0.5]).unwrap();
            }
            if tick == 140 {
                player.set_flight(true).unwrap();
            }
            if tick == 220 {
                player.set_flight(false).unwrap();
            }
            let state = PredictionState::capture(&player).unwrap();
            assert!(state.as_bytes().len() <= MAX_PREDICTION_BYTES);
            let mut restored = PredictionState::decode(state.as_bytes())
                .unwrap()
                .to_player()
                .unwrap();
            let keys = Keys {
                fwd: tick % 5 != 0,
                left: tick % 13 == 0,
                jump: tick % 17 < 3,
                sneak: tick % 19 < 4,
                sprint: tick % 3 == 0,
                ..Keys::default()
            };
            assert_eq!(player.tick(&query, keys), restored.tick(&query, keys));
            assert_eq!(
                PredictionState::capture(&player).unwrap(),
                PredictionState::capture(&restored).unwrap()
            );
        }
    }
    #[test]
    fn public_state_excludes_private_grants_and_rejects_malformed_data() {
        let mut player = Player::new([1.0, 2.0, 3.0]);
        let state = PredictionState::capture(&player).unwrap();
        player.debug_permission = true;
        assert_eq!(state, PredictionState::capture(&player).unwrap());
        for len in 0..state.as_bytes().len() {
            assert!(PredictionState::decode(&state.as_bytes()[..len]).is_err());
        }
        let mut bytes = state.as_bytes().to_vec();
        bytes[6..14].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
        assert!(PredictionState::decode(&bytes).is_err());
        let mut bytes = state.as_bytes().to_vec();
        bytes[66] = 2;
        assert!(PredictionState::decode(&bytes).is_err());
        let mut bytes = state.as_bytes().to_vec();
        bytes[4] = 99;
        assert!(PredictionState::decode(&bytes).is_err());
        let mut bytes = state.as_bytes().to_vec();
        bytes.push(0);
        assert!(PredictionState::decode(&bytes).is_err());
        assert!(PredictionState::decode(&[0; MAX_PREDICTION_BYTES + 1]).is_err());
    }
}
