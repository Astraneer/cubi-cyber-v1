//! Deterministic 20Hz movement and world interactions. The caller owns the world
//! and commits transactions; simulation queries never generate unknown pages.
#![forbid(unsafe_code)]
pub use camera::wrap_degrees;

mod camera;
pub mod collision;
pub mod entities;
mod interaction;
mod motion;
pub mod prediction;
mod query;
pub mod replay;

pub use camera::{CameraMode, CameraPose, CameraSettings, SelfBody, camera_clip};
use collision::Solids;
pub use interaction::*;
pub use motion::{Keys, Motion};
pub use query::{QueryWorld, WorldQuery};
pub use rules::shapes::Aabb;
use std::ops::{Deref, DerefMut};

pub const TICKS_PER_SECOND: u32 = 20;
pub const TICK_SECONDS: f64 = 1.0 / TICKS_PER_SECOND as f64;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum GameMode {
    #[default]
    Survival = 0,
    Creative = 1,
    Spectator = 3,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Permissions {
    pub server_debug: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlError {
    DebugRequired,
    InvalidPosition,
}
/// One shared flight rule for local prediction and authoritative validation.
pub const fn may_fly(mode: GameMode) -> bool {
    matches!(
        mode,
        GameMode::Survival | GameMode::Creative | GameMode::Spectator
    )
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TickOutcome {
    pub waiting_for_world: bool,
    pub unknown_boundary: bool,
    pub fall_damage: u32,
    pub moved: bool,
}

/// Player state has one owner. Movement sees a read-only query; edits use explicit
/// transaction validation in the interaction module.
pub struct Player {
    motion: Motion,
    pub health: f32,
    debug_permission: bool,
    pub self_body: SelfBody,
}
impl Deref for Player {
    type Target = Motion;
    fn deref(&self) -> &Self::Target {
        &self.motion
    }
}
impl DerefMut for Player {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.motion
    }
}
impl Player {
    pub fn new(position: [f64; 3]) -> Self {
        Self {
            motion: Motion::new(position),
            health: 20.0,
            debug_permission: false,
            self_body: SelfBody::default(),
        }
    }
    pub fn set_mode(&mut self, mode: GameMode) {
        let was = self.motion.mode;
        self.motion.mode = mode;
        if mode == GameMode::Spectator {
            self.motion.set_flying(true);
        } else if was == GameMode::Spectator {
            self.motion.noclip = false;
            self.motion.flying = true;
        }
    }
    pub fn set_flight(&mut self, enabled: bool) -> Result<(), ControlError> {
        if may_fly(self.motion.mode) {
            self.motion.set_flight(enabled);
        }
        Ok(())
    }
    pub fn set_debug_noclip(
        &mut self,
        enabled: bool,
        permissions: Permissions,
    ) -> Result<(), ControlError> {
        if enabled && !permissions.server_debug {
            return Err(ControlError::DebugRequired);
        }
        self.debug_permission = permissions.server_debug;
        self.motion.set_flying(enabled);
        Ok(())
    }
    pub const fn debug_permission(&self) -> bool {
        self.debug_permission
    }
    pub fn revoke_debug_permission(&mut self) {
        self.debug_permission = false;
        if self.motion.mode != GameMode::Spectator && self.motion.noclip {
            self.motion.set_flying(false);
        }
    }
    pub fn turn(&mut self, dx: f64, dy: f64, sensitivity: f64) {
        if dx.is_finite() && dy.is_finite() && sensitivity.is_finite() {
            self.motion.turn(dx, dy, sensitivity.clamp(0.0, 1.0));
        }
    }
    pub fn teleport(&mut self, position: [f64; 3]) -> Result<(), ControlError> {
        if !valid_position(position) {
            return Err(ControlError::InvalidPosition);
        }
        self.motion.teleport(position);
        self.motion.on_ground = false;
        self.motion.support = None;
        self.self_body.snap(self.motion.yaw);
        Ok(())
    }
    pub fn tick(&mut self, world: &impl QueryWorld, keys: Keys) -> TickOutcome {
        self.tick_using(world, keys, false)
    }
    pub fn tick_using(
        &mut self,
        world: &impl QueryWorld,
        keys: Keys,
        using_item: bool,
    ) -> TickOutcome {
        if !valid_position(self.motion.pos)
            || self
                .motion
                .vel
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 64.0)
            || !self.motion.yaw.is_finite()
            || !self.motion.pitch.is_finite()
        {
            return TickOutcome {
                waiting_for_world: true,
                ..TickOutcome::default()
            };
        }
        if self.motion.noclip && self.motion.mode != GameMode::Spectator && !self.debug_permission {
            self.motion.set_flying(false);
        }
        let mut query = query::CollisionQuery::new(world);
        if !self.motion.noclip && query::body_has_unknown(world, self.motion.bbox()) {
            return TickOutcome {
                waiting_for_world: true,
                unknown_boundary: true,
                ..TickOutcome::default()
            };
        }
        let mut outcome = self.tick_solids_using(&mut query, keys, using_item);
        outcome.unknown_boundary = query.unknown;
        outcome
    }
    /// Query-trait entry for independent geometric golden tests and actor types.
    /// Production world movement should use tick(), which enforces Unknown policy.
    pub fn tick_solids(&mut self, world: &mut impl Solids, keys: Keys) -> TickOutcome {
        self.tick_solids_using(world, keys, false)
    }
    fn tick_solids_using(
        &mut self,
        world: &mut impl Solids,
        keys: Keys,
        using_item: bool,
    ) -> TickOutcome {
        if self.health <= 0.0 {
            return TickOutcome::default();
        }
        if self.motion.noclip && self.motion.mode != GameMode::Spectator && !self.debug_permission {
            self.motion.set_flying(false);
        }
        self.motion.camera_tick();
        let before = self.motion.pos;
        self.motion.tick(world, keys, using_item);
        self.self_body.tick(&self.motion);
        let mut damage = 0;
        if self.motion.mode == GameMode::Survival
            && !self.motion.noclip
            && let Some((tick, distance)) = self.motion.landed
            && tick == self.motion.ticks
        {
            damage = fall_damage(distance, 1.0);
            self.health = (self.health - damage as f32).max(0.0);
        }
        TickOutcome {
            moved: self.motion.pos != before,
            fall_damage: damage,
            ..TickOutcome::default()
        }
    }
    pub fn bbox(&self) -> Aabb {
        self.motion.bbox()
    }
    pub fn eye(&self, alpha: f64) -> [f64; 3] {
        self.motion.eye(alpha.clamp(0.0, 1.0))
    }
    pub fn sneaking(&self) -> bool {
        self.motion.sneaking()
    }
    pub fn eye_height(&self) -> f32 {
        self.motion.eye_height()
    }
    pub fn look(&self) -> [f64; 3] {
        self.motion.look()
    }
    pub fn view(&self, alpha: f64) -> [f64; 10] {
        self.motion.view(alpha.clamp(0.0, 1.0))
    }
    pub fn swing_hand(&mut self) {
        self.motion.swing_hand();
    }
    pub fn state_hash(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut eat = |b: &[u8]| {
            for byte in b {
                h = (h ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
            }
        };
        for value in self
            .motion
            .pos
            .iter()
            .chain(self.motion.prev.iter())
            .chain(self.motion.vel.iter())
        {
            eat(&value.to_bits().to_le_bytes());
        }
        for value in [
            self.motion.yaw,
            self.motion.pitch,
            self.motion.fall,
            self.motion.fwd_in,
            self.motion.side,
            self.motion.fwd,
            self.motion.hspeed,
            self.motion.prev_hspeed,
            self.motion.stride,
            self.motion.prev_stride,
            self.motion.cam_y,
            self.motion.last_cam_y,
            self.motion.fov,
            self.motion.last_fov,
            self.motion.swing,
            self.motion.last_swing,
            self.health,
        ] {
            eat(&value.to_bits().to_le_bytes());
        }
        for value in [
            self.motion.on_ground,
            self.motion.h_coll,
            self.motion.soft,
            self.motion.sprinting,
            self.motion.sneak_in,
            self.motion.crouch,
            self.motion.flying,
            self.motion.noclip,
            self.motion.in_water,
            self.motion.in_lava,
            self.motion.eye_water,
            self.motion.swinging,
            self.debug_permission,
        ] {
            eat(&[u8::from(value)]);
        }
        eat(&[self.motion.mode as u8]);
        eat(&self.motion.ticks.to_le_bytes());
        for value in [
            self.motion.jump_cd,
            self.motion.dtap,
            self.motion.swing_ticks,
        ] {
            eat(&value.to_le_bytes());
        }
        for value in [self.motion.fluid_h, self.motion.lava_h] {
            eat(&value.to_bits().to_le_bytes());
        }
        match self.motion.support {
            Some(p) => {
                eat(&[1]);
                for c in p {
                    eat(&c.to_le_bytes());
                }
            }
            None => eat(&[0]),
        }
        match self.motion.landed {
            Some((t, d)) => {
                eat(&[1]);
                eat(&t.to_le_bytes());
                eat(&d.to_bits().to_le_bytes());
            }
            None => eat(&[0]),
        }
        h
    }
}
fn valid_position(position: [f64; 3]) -> bool {
    position
        .iter()
        .all(|v| v.is_finite() && v.abs() < 30_000_000.0)
}
pub fn fall_damage(distance: f32, multiplier: f32) -> u32 {
    ((distance - 3.0f32) * multiplier).ceil().max(0.0) as u32
}

/// Reproducible movement trace for native/WASM comparison. Numeric correctness
/// is established separately by Java/donor golden tests, not by this self-check.
pub fn determinism_trace() -> [u64; 256] {
    let query = |p: [i32; 3]| {
        world::CellRead::Known(core_types::Cell::from_parts(
            if p[1] < 0 || p[2] == 8 && p[1] < 5 {
                3
            } else if p[0] == 2 && (0..4).contains(&p[1]) && (1..6).contains(&p[2]) {
                15
            } else if p[0] == 0 && p[2] == 7 && (0..5).contains(&p[1]) {
                53
            } else {
                0
            },
            if p[0] == 0 && p[2] == 7 && (0..5).contains(&p[1]) {
                2
            } else {
                0
            },
        ))
    };
    let mut player = Player::new([0.5, 0.0, 0.5]);
    std::array::from_fn(|tick| {
        if tick == 64 {
            player.teleport([2.5, 1.0, 2.5]).expect("fixed water pose");
        }
        if tick == 128 {
            player.teleport([0.5, 6.0, 0.5]).expect("fixed flight pose");
            player.set_flight(true).expect("survival flight allowed");
        }
        if tick == 184 {
            player.set_flight(false).expect("flight off");
        }
        let keys = match tick {
            0..=63 => Keys {
                fwd: true,
                sprint: tick > 20,
                jump: tick % 19 == 0,
                ..Keys::default()
            },
            64..=95 => Keys {
                jump: true,
                ..Keys::default()
            },
            96..=127 => Keys {
                sneak: true,
                ..Keys::default()
            },
            128..=155 => Keys {
                jump: true,
                fwd: true,
                ..Keys::default()
            },
            156..=183 => Keys {
                left: true,
                sprint: true,
                ..Keys::default()
            },
            _ => Keys::default(),
        };
        if tick % 11 == 0 {
            player.turn(7.0, -1.0, 0.5);
        }
        player.tick(&query, keys);
        player.state_hash()
    })
}
