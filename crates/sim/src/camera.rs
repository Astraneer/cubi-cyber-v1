//! Interpolated camera and reusable collision probes; no per-frame model rebuild.
use crate::{Motion, Player, QueryWorld, collision::Solids};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CameraMode {
    #[default]
    FirstPerson,
    RightShoulder,
}
#[derive(Clone, Copy, Debug)]
pub struct CameraSettings {
    pub distance: f64,
    pub shoulder: f64,
    pub height: f64,
}
impl Default for CameraSettings {
    fn default() -> Self {
        Self {
            distance: 4.0,
            shoulder: 0.65,
            height: 0.15,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct CameraPose {
    pub eye: [f64; 3],
    pub look: [f64; 3],
    pub self_visible: bool,
    pub fov_multiplier: f32,
}
impl Player {
    pub fn camera(&self, alpha: f64, mode: CameraMode, world: &impl QueryWorld) -> CameraPose {
        self.camera_with_settings(alpha, mode, world, CameraSettings::default())
    }
    pub fn camera_with_settings(
        &self,
        alpha: f64,
        mode: CameraMode,
        world: &impl QueryWorld,
        settings: CameraSettings,
    ) -> CameraPose {
        let view = self.view(alpha);
        let base = [view[0], view[1], view[2]];
        let look = self.look();
        if mode == CameraMode::FirstPerson {
            return CameraPose {
                eye: base,
                look,
                self_visible: false,
                fov_multiplier: view[5] as f32,
            };
        }
        let yaw = f64::from(self.yaw).to_radians();
        let right = [-libm::cos(yaw), 0.0, -libm::sin(yaw)];
        let delta = std::array::from_fn(|axis| {
            -look[axis] * settings.distance
                + right[axis] * settings.shoulder
                + if axis == 1 { settings.height } else { 0.0 }
        });
        let length = norm(delta);
        let direction = normalize(delta);
        let distance = clip_query(world, base, direction, length);
        let eye = std::array::from_fn(|axis| base[axis] + direction[axis] * distance);
        let target: [f64; 3] = std::array::from_fn(|axis| base[axis] + look[axis] * 32.0);
        CameraPose {
            eye,
            look: normalize(std::array::from_fn(|axis| target[axis] - eye[axis])),
            self_visible: true,
            fov_multiplier: view[5] as f32,
        }
    }
}
fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}
fn normalize(v: [f64; 3]) -> [f64; 3] {
    let n = norm(v);
    if n > 1e-12 {
        [v[0] / n, v[1] / n, v[2] / n]
    } else {
        [0.0, 0.0, 1.0]
    }
}
/// Camera path for real loaded-cell queries uses registry slices directly.
/// It allocates neither a scratch vector nor per-frame model geometry.
fn clip_query(world: &impl QueryWorld, eye: [f64; 3], direction: [f64; 3], max: f64) -> f64 {
    if !eye
        .iter()
        .chain(direction.iter())
        .all(|value| value.is_finite())
        || !(0.0..=16.0).contains(&max)
    {
        return 0.0;
    }
    let mut distance = max;
    for corner in 0..8 {
        let start = std::array::from_fn(|axis| {
            eye[axis] + if corner & (1 << axis) == 0 { -0.1 } else { 0.1 }
        });
        let end = std::array::from_fn(|axis| start[axis] + direction[axis] * distance);
        for (cell, _) in crate::interaction::ray_cells(start, end) {
            let shapes = match world.cell(cell) {
                world::CellRead::Unknown => rules::shapes::CUBE,
                world::CellRead::Known(cell) => rules::blocks::collision_of(cell.raw()),
            };
            let offset = cell.map(f64::from);
            if let Some((t, _)) = shapes
                .iter()
                .filter_map(|shape| {
                    crate::interaction::segment_box(start, end, shape.moved(offset))
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
            {
                distance = distance.min((t * distance).max(0.0));
                break;
            }
        }
    }
    distance
}

/// Corner rays keep a camera cube of half-width0.1 outside all collision shapes.
pub fn camera_clip(world: &mut impl Solids, eye: [f64; 3], direction: [f64; 3], max: f64) -> f64 {
    if !eye
        .iter()
        .chain(direction.iter())
        .all(|value| value.is_finite())
        || !(0.0..=16.0).contains(&max)
    {
        return 0.0;
    }
    let mut distance = max;
    let mut shapes = Vec::with_capacity(32);
    for corner in 0..8 {
        let start = std::array::from_fn(|axis| {
            eye[axis] + if corner & (1 << axis) == 0 { -0.1 } else { 0.1 }
        });
        let end = std::array::from_fn(|axis| start[axis] + direction[axis] * distance);
        for (cell, entry) in crate::interaction::ray_cells(start, end) {
            if entry > 1.0 {
                break;
            }
            shapes.clear();
            world.shapes_at(cell[0], cell[1], cell[2], &mut shapes);
            if let Some((t, _)) = shapes
                .iter()
                .filter_map(|shape| crate::interaction::segment_box(start, end, *shape))
                .min_by(|a, b| a.0.total_cmp(&b.0))
            {
                distance = distance.min((t * distance).max(0.0));
                break;
            }
        }
    }
    distance
}
/// Per-tick body animation; a renderer uploads only this pose to an existing model.
#[derive(Clone, Copy, Default)]
pub struct SelfBody {
    body: f32,
    previous_body: f32,
    limb: f32,
    previous_limb: f32,
    amount: f32,
    previous_amount: f32,
    age: u64,
}
impl SelfBody {
    pub fn snap(&mut self, yaw: f32) {
        self.body = yaw;
        self.previous_body = yaw;
    }
    pub(crate) fn tick(&mut self, player: &Motion) {
        let dx = (player.pos[0] - player.prev[0]) as f32;
        let dz = (player.pos[2] - player.prev[2]) as f32;
        self.age = self.age.wrapping_add(1);
        self.previous_body = self.body;
        self.body = turn_body(self.body, player.yaw, dx, dz, player.swinging);
        self.previous_limb = self.limb;
        self.previous_amount = self.amount;
        self.amount += (((dx * dx + dz * dz).sqrt() * 4.0).min(1.0) - self.amount) * 0.4;
        self.limb += self.amount;
    }
    pub fn pose(&self, alpha: f64) -> [f32; 4] {
        let a = alpha.clamp(0.0, 1.0) as f32;
        [
            self.previous_body + wrap_degrees(self.body - self.previous_body) * a,
            self.previous_limb + (self.limb - self.previous_limb) * a,
            self.previous_amount + (self.amount - self.previous_amount) * a,
            self.age as f32 + a,
        ]
    }
}
pub fn wrap_degrees(value: f32) -> f32 {
    let wrapped = value.rem_euclid(360.0);
    if wrapped >= 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}
pub fn turn_body(body: f32, yaw: f32, dx: f32, dz: f32, swinging: bool) -> f32 {
    let mut target = body;
    if dx * dx + dz * dz > 0.0025 {
        let h = libm::atan2f(dz, dx).to_degrees() - 90.0;
        let diff = (wrap_degrees(yaw) - h).abs();
        target = if 95.0 < diff && diff < 265.0 {
            h - 180.0
        } else {
            h
        };
    }
    if swinging {
        target = yaw;
    }
    let mut result = body + wrap_degrees(target - body) * 0.3;
    let g = wrap_degrees(yaw - result).clamp(-75.0, 75.0);
    result = yaw - g;
    if g * g > 2500.0 {
        result += g * 0.2;
    }
    result
}
