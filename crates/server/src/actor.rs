//! Server-private, versioned continuation of the shared deterministic player.
//!
//! Public positions are deliberately insufficient for restart: fall distance,
//! fluid state, support, input double-tap and jump cooldowns all affect the next
//! authoritative tick. Reusable collision scratch buffers are not state.
use core_types::SpaceId;
use net::{PlayerId, PlayerState};
use save::SavedPlayer;
use sim::{GameMode, Permissions, Player};

const MAGIC: &[u8; 4] = b"PLYR";
const VERSION: u16 = 1;
const MAX_BYTES: usize = 4096;
const POSITION_LIMIT: f64 = 30_000_000.0;

pub(crate) fn capture(
    identity: PlayerId,
    space: SpaceId,
    input_sequence: u64,
    player: &Player,
) -> SavedPlayer {
    let state = PlayerState {
        identity,
        space,
        input_sequence,
        position: player.pos,
        velocity: player.vel,
        yaw: player.yaw,
        pitch: player.pitch,
        health: player.health,
        mode: player.mode as u8,
        on_ground: player.on_ground,
        flying: player.flying,
        noclip: player.noclip,
    };
    let mut out = Writer(Vec::with_capacity(256));
    out.0.extend_from_slice(MAGIC);
    out.0.extend_from_slice(&VERSION.to_le_bytes());
    out.bool(player.debug_permission());
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
        out.0.extend_from_slice(&value.to_le_bytes());
    }
    out.f64(player.fluid_h);
    out.f64(player.lava_h);
    out.0.extend_from_slice(&player.ticks.to_le_bytes());
    out.bool(player.support.is_some());
    if let Some(position) = player.support {
        for axis in position {
            out.0.extend_from_slice(&axis.to_le_bytes());
        }
    }
    out.bool(player.landed.is_some());
    if let Some((tick, distance)) = player.landed {
        out.0.extend_from_slice(&tick.to_le_bytes());
        out.f32(distance);
    }
    debug_assert!(out.0.len() <= MAX_BYTES);
    SavedPlayer {
        state,
        place_ready_at: core_types::Tick(0),
        break_ready_at: core_types::Tick(0),
        inventory: Vec::new(),
        simulation: out.0,
    }
}

pub(crate) fn restore(saved: &SavedPlayer, allow_debug: bool) -> Result<Player, &'static str> {
    validate_public(&saved.state)?;
    if saved.simulation.len() > MAX_BYTES {
        return Err("player continuation exceeds bound");
    }
    let mut input = Reader {
        bytes: &saved.simulation,
        offset: 0,
    };
    if &input.array::<4>()? != MAGIC {
        return Err("invalid player continuation magic");
    }
    if u16::from_le_bytes(input.array()?) != VERSION {
        return Err("explicit player continuation migration required");
    }
    let debug_permission = input.bool()?;
    let mut player = Player::new(saved.state.position);
    // Permission setup itself changes motion; do it before installing the exact
    // saved continuation rather than erasing fall/fluid/support state afterward.
    player
        .set_debug_noclip(
            false,
            Permissions {
                server_debug: allow_debug && debug_permission,
            },
        )
        .map_err(|_| "invalid player permission")?;
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
    player.pos = saved.state.position;
    player.vel = saved.state.velocity;
    player.yaw = saved.state.yaw;
    player.pitch = saved.state.pitch;
    player.health = saved.state.health;
    player.mode = game_mode(saved.state.mode)?;
    player.on_ground = saved.state.on_ground;
    player.flying = saved.state.flying;
    player.noclip = saved.state.noclip;
    if player.noclip && player.mode != GameMode::Spectator && !(allow_debug && debug_permission) {
        player.revoke_debug_permission();
    }
    player.self_body.snap(player.yaw);
    Ok(player)
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
fn validate_public(state: &PlayerState) -> Result<(), &'static str> {
    game_mode(state.mode)?;
    if !valid_position(state.position)
        || state
            .velocity
            .iter()
            .any(|axis| !axis.is_finite() || axis.abs() > 64.0)
        || !state.yaw.is_finite()
        || !state.pitch.is_finite()
        || state.pitch.abs() > 90.0
        || !state.health.is_finite()
        || !(0.0..=20.0).contains(&state.health)
        || state.noclip && !state.flying
    {
        return Err("invalid public player continuation");
    }
    Ok(())
}
struct Writer(Vec<u8>);
impl Writer {
    fn f32(&mut self, value: f32) {
        self.0.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    fn f64(&mut self, value: f64) {
        self.0.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    fn bool(&mut self, value: bool) {
        self.0.push(u8::from(value));
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
    use core_types::Cell;
    use sim::Keys;
    use world::CellRead;

    fn query(position: [i32; 3]) -> CellRead {
        let id = if position[1] < 0 || position[2] == 4 && (0..4).contains(&position[1]) {
            rules::ids::STONE
        } else if (3..7).contains(&position[0]) && (0..3).contains(&position[1]) {
            rules::ids::WATER
        } else if (-6..-2).contains(&position[0]) && (0..2).contains(&position[1]) {
            rules::ids::LAVA
        } else {
            rules::ids::AIR
        };
        CellRead::Known(Cell::from_parts(id, 0))
    }
    fn save(player: &Player) -> SavedPlayer {
        capture(PlayerId([9; 16]), SpaceId(0), 47, player)
    }

    #[test]
    fn restart_preserves_next_tick_in_flight_collision_fall_and_fluids() {
        let mut observed_water = false;
        let mut observed_lava = false;
        let mut observed_fall = false;
        let mut observed_collision = false;
        let mut observed_cooldown = false;
        for (position, flying) in [
            ([0.5, 12.0, 0.5], false),
            ([0.5, 0.0, 3.4], false),
            ([4.5, 0.0, 0.5], false),
            ([-4.5, 0.0, 0.5], false),
            ([0.5, 6.0, 0.5], true),
        ] {
            let mut original = Player::new(position);
            original.set_flight(flying).unwrap();
            for tick in 0..64 {
                original.swing_hand();
                let encoded = save(&original);
                assert!(encoded.simulation.len() < 512);
                let mut restarted = restore(&encoded, true).unwrap();
                assert_eq!(original.state_hash(), restarted.state_hash());
                let keys = Keys {
                    fwd: true,
                    left: tick % 4 == 0,
                    jump: tick % 11 == 0,
                    sneak: tick % 9 == 0,
                    sprint: tick % 3 == 0,
                    ..Keys::default()
                };
                assert_eq!(original.tick(&query, keys), restarted.tick(&query, keys));
                assert_eq!(original.state_hash(), restarted.state_hash());
                observed_water |= original.in_water;
                observed_lava |= original.in_lava;
                observed_fall |= original.fall > 0.0;
                observed_collision |= original.h_coll;
                observed_cooldown |= original.jump_cd > 0;
            }
        }
        assert!(observed_water && observed_lava && observed_fall);
        assert!(observed_collision && observed_cooldown);
    }

    #[test]
    fn debug_permission_is_exact_and_revoked_without_authority() {
        let mut debug = Player::new([0.5, 4.0, 0.5]);
        debug
            .set_debug_noclip(true, Permissions { server_debug: true })
            .unwrap();
        let encoded = save(&debug);
        assert_eq!(
            debug.state_hash(),
            restore(&encoded, true).unwrap().state_hash()
        );
        let revoked = restore(&encoded, false).unwrap();
        assert!(!revoked.noclip && !revoked.flying && !revoked.debug_permission());

        // An enabled permission remains part of state when H is switched off.
        debug
            .set_debug_noclip(false, Permissions { server_debug: true })
            .unwrap();
        assert!(debug.debug_permission() && !debug.noclip);
        assert_eq!(
            debug.state_hash(),
            restore(&save(&debug), true).unwrap().state_hash()
        );

        let mut spectator = Player::new([0.5, 4.0, 0.5]);
        spectator.set_mode(GameMode::Spectator);
        let restored = restore(&save(&spectator), false).unwrap();
        assert!(restored.noclip && restored.flying);
        assert_eq!(spectator.state_hash(), restored.state_hash());
    }

    #[test]
    fn malformed_or_unversioned_continuations_never_load() {
        let mut original = Player::new([0.5, 4.0, 0.5]);
        original.support = Some([0, -1, 0]);
        original.landed = Some((0, 2.0));
        let encoded = save(&original);
        for len in 0..encoded.simulation.len() {
            let mut short = encoded.clone();
            short.simulation.truncate(len);
            assert!(restore(&short, true).is_err(), "prefix {len}");
        }
        let mut trailing = encoded.clone();
        trailing.simulation.push(0);
        assert!(restore(&trailing, true).is_err());
        let mut bad_version = encoded.clone();
        bad_version.simulation[4] = 2;
        assert!(restore(&bad_version, true).is_err());
        let mut invalid_bool = encoded.clone();
        invalid_bool.simulation[6] = 2;
        assert!(restore(&invalid_bool, true).is_err());
        let mut nonfinite = encoded.clone();
        nonfinite.simulation[7..15].copy_from_slice(&f64::NAN.to_le_bytes());
        assert!(restore(&nonfinite, true).is_err());
        let mut bad_public = encoded.clone();
        bad_public.state.position[0] = f64::INFINITY;
        assert!(restore(&bad_public, true).is_err());
        let mut bad_mode = encoded.clone();
        bad_mode.state.mode = 2;
        assert!(restore(&bad_mode, true).is_err());
        let mut oversized = encoded;
        oversized.simulation.resize(MAX_BYTES + 1, 0);
        assert!(restore(&oversized, true).is_err());
    }
}
