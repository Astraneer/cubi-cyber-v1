//! Six immutable cuboids for the local avatar. Only object matrices change in a frame.
//!
//! This is an original, deliberately simple model: tinted approved white-wool
//! texels provide skin, hair and clothing without a downloaded skin or new atlas.
use render::terrain_mesh::{Face, PageMesh, QuadDescriptor};
use sim::{CameraMode, GameMode, Player};

pub const PART_COUNT: usize = 6;
pub const PART_EXTENTS: [[u8; 3]; PART_COUNT] = [
    [8, 8, 8],
    [8, 12, 4],
    [3, 12, 4],
    [3, 12, 4],
    [4, 12, 4],
    [4, 12, 4],
];
/// The standing skeleton is 32 pixels tall, matching the 1.8-block player body.
pub const MODEL_HEIGHT: f32 = 1.8;
const PIXEL: f32 = MODEL_HEIGHT / 32.0;
const SKIN: u32 = rgba(218, 171, 128);
const HAIR: u32 = rgba(76, 48, 35);
const SHIRT: u32 = rgba(73, 157, 183);
const TROUSERS: u32 = rgba(68, 77, 119);
const MATERIAL: u16 = rules::ids::L_WOOL_WHITE as u16;

pub type ModelMatrix = [[f32; 4]; 4];

/// Stable order shared by the startup meshes and every subsequent pose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum BodyPart {
    Head,
    Torso,
    LeftArm,
    RightArm,
    LeftLeg,
    RightLeg,
}
impl BodyPart {
    pub const ALL: [Self; PART_COUNT] = [
        Self::Head,
        Self::Torso,
        Self::LeftArm,
        Self::RightArm,
        Self::LeftLeg,
        Self::RightLeg,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelfPose {
    /// Column-major affine matrices, in `BodyPart::ALL` order.
    pub transforms: [ModelMatrix; PART_COUNT],
    /// First person and spectator mode hide the existing GPU objects.
    pub visible: bool,
}

/// Startup owner. Upload each mesh once, retain its object handle, and use
/// `poses` to update its transform/enabled flag without replacing descriptors.
pub struct SelfModel {
    meshes: [PageMesh; PART_COUNT],
}
impl Default for SelfModel {
    fn default() -> Self {
        Self::new()
    }
}
impl SelfModel {
    pub fn new() -> Self {
        let meshes = BodyPart::ALL.map(|part| {
            let colors = match part {
                BodyPart::Head => [SKIN, HAIR, HAIR, SKIN, HAIR, HAIR],
                BodyPart::Torso => [SHIRT; 6],
                BodyPart::LeftArm | BodyPart::RightArm => [SKIN; 6],
                BodyPart::LeftLeg | BodyPart::RightLeg => [TROUSERS; 6],
            };
            cuboid(PART_EXTENTS[part as usize], colors)
        });
        Self { meshes }
    }
    pub const fn meshes(&self) -> &[PageMesh; PART_COUNT] {
        &self.meshes
    }

    /// Interpolate feet and the simulation-owned body animation. The same
    /// immutable geometry supports body yaw, head look, gait and an arm swing.
    /// Invalid externally-mutated player coordinates produce a hidden finite pose.
    pub fn poses(player: &Player, alpha: f64, mode: CameraMode) -> SelfPose {
        if player
            .pos
            .iter()
            .chain(player.prev.iter())
            .any(|value| !value.is_finite() || value.abs() > 30_000_000.0)
            || !player.yaw.is_finite()
            || !player.pitch.is_finite()
        {
            return SelfPose {
                transforms: [identity(); PART_COUNT],
                visible: false,
            };
        }
        let alpha = if alpha.is_finite() {
            alpha.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let feet = std::array::from_fn(|axis| {
            (player.prev[axis] + (player.pos[axis] - player.prev[axis]) * alpha) as f32
        });
        let [body_yaw, limb, amount, age] = player.self_body.pose(alpha);
        let body_yaw = wrap_degrees(finite(body_yaw));
        let stride = libm::cosf(finite(limb) * 0.6662) * finite(amount).clamp(0.0, 1.0);
        let idle = libm::sinf(finite(age) * 0.067) * 0.025;
        let swing_delta = finite(player.swing - player.last_swing).rem_euclid(1.0);
        let swing = (finite(player.last_swing) + swing_delta * alpha as f32).rem_euclid(1.0);
        let attack = libm::sinf(swing * std::f32::consts::PI) * 1.5;
        // A crouched body remains inside the simulation's 1.5-block height.
        // Its six rigid parts are retained rather than rebuilt for that pose.
        let height_scale = if player.crouch { 5.0 / 6.0 } else { 1.0 };
        let root = multiply(
            translate(feet),
            multiply(
                rotate_y(-body_yaw.to_radians()),
                scale([PIXEL, PIXEL * height_scale, PIXEL]),
            ),
        );
        let head_yaw = wrap_degrees(wrap_degrees(player.yaw) - body_yaw);
        let transforms = [
            part_matrix(
                root,
                [0.0, 24.0, 0.0],
                multiply(
                    rotate_y(-head_yaw.to_radians()),
                    rotate_x(player.pitch.clamp(-90.0, 90.0).to_radians()),
                ),
                [-4.0, 0.0, -4.0],
            ),
            part_matrix(root, [0.0, 12.0, 0.0], identity(), [-4.0, 0.0, -2.0]),
            part_matrix(
                root,
                [5.5, 24.0, 0.0],
                rotate_x(stride + idle),
                [-1.5, -12.0, -2.0],
            ),
            part_matrix(
                root,
                [-5.5, 24.0, 0.0],
                rotate_x(-stride - idle - attack),
                [-1.5, -12.0, -2.0],
            ),
            part_matrix(
                root,
                [2.0, 12.0, 0.0],
                rotate_x(-stride * 1.4),
                [-2.0, -12.0, -2.0],
            ),
            part_matrix(
                root,
                [-2.0, 12.0, 0.0],
                rotate_x(stride * 1.4),
                [-2.0, -12.0, -2.0],
            ),
        ];
        SelfPose {
            transforms,
            visible: mode == CameraMode::RightShoulder && player.mode != GameMode::Spectator,
        }
    }
}

const fn rgba(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16) | 0xff00_0000
}
fn finite(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}
fn wrap_degrees(value: f32) -> f32 {
    (value + 180.0).rem_euclid(360.0) - 180.0
}
fn cuboid([width, height, depth]: [u8; 3], colors: [u32; 6]) -> PageMesh {
    let mut mesh = PageMesh::with_capacity([6, 0, 0]);
    for face in Face::ALL {
        let (origin, size) = match face {
            Face::Down => ([0, 0, 0], [width, depth]),
            Face::Up => ([0, height, depth], [width, depth]),
            Face::North => ([width, 0, 0], [width, height]),
            Face::South => ([0, 0, depth], [width, height]),
            Face::West => ([0, 0, 0], [depth, height]),
            Face::East => ([width, 0, depth], [depth, height]),
        };
        mesh.opaque.push(
            QuadDescriptor::new(origin, face, size, MATERIAL, [3; 4], colors[face as usize])
                .expect("fixed model cuboids fit the descriptor's 16-pixel bounds"),
        );
    }
    mesh
}
fn part_matrix(
    root: ModelMatrix,
    pivot: [f32; 3],
    rotation: ModelMatrix,
    offset: [f32; 3],
) -> ModelMatrix {
    multiply(
        root,
        multiply(translate(pivot), multiply(rotation, translate(offset))),
    )
}
const fn identity() -> ModelMatrix {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}
fn translate(position: [f32; 3]) -> ModelMatrix {
    let mut result = identity();
    result[3][..3].copy_from_slice(&position);
    result
}
fn scale(factors: [f32; 3]) -> ModelMatrix {
    let mut result = identity();
    for axis in 0..3 {
        result[axis][axis] = factors[axis];
    }
    result
}
fn rotate_x(angle: f32) -> ModelMatrix {
    let (sine, cosine) = (libm::sinf(angle), libm::cosf(angle));
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, cosine, sine, 0.0],
        [0.0, -sine, cosine, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}
fn rotate_y(angle: f32) -> ModelMatrix {
    let (sine, cosine) = (libm::sinf(angle), libm::cosf(angle));
    [
        [cosine, 0.0, -sine, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [sine, 0.0, cosine, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}
fn multiply(a: ModelMatrix, b: ModelMatrix) -> ModelMatrix {
    std::array::from_fn(|column| {
        std::array::from_fn(|row| (0..4).map(|axis| a[axis][row] * b[column][axis]).sum())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{alloc::System, hint::black_box};

    #[global_allocator]
    static ALLOCATOR: telemetry::CountingAllocator<System> =
        telemetry::CountingAllocator::new(System);

    fn point(matrix: ModelMatrix, local: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|row| {
            matrix[3][row]
                + (0..3)
                    .map(|column| matrix[column][row] * local[column])
                    .sum::<f32>()
        })
    }
    fn close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
    }
    fn bounds(model: &SelfModel, pose: SelfPose) -> ([f32; 3], [f32; 3]) {
        let mut low = [f32::INFINITY; 3];
        let mut high = [f32::NEG_INFINITY; 3];
        for (mesh, matrix) in model.meshes().iter().zip(pose.transforms) {
            for quad in mesh.iter() {
                for vertex in 0..4 {
                    let transformed = point(matrix, quad.vertex(vertex).map(f32::from));
                    for axis in 0..3 {
                        low[axis] = low[axis].min(transformed[axis]);
                        high[axis] = high[axis].max(transformed[axis]);
                    }
                }
            }
        }
        (low, high)
    }
    #[test]
    fn startup_contains_six_closed_outward_cuboids_with_valid_materials() {
        let model = SelfModel::new();
        let dimensions = [
            [8, 8, 8],
            [8, 12, 4],
            [3, 12, 4],
            [3, 12, 4],
            [4, 12, 4],
            [4, 12, 4],
        ];
        for (mesh, extent) in model.meshes().iter().zip(dimensions) {
            assert_eq!(mesh.opaque.len(), 6);
            assert!(mesh.cutout.is_empty() && mesh.water.is_empty());
            for (quad, face) in mesh.opaque.iter().zip(Face::ALL) {
                assert_eq!(quad.face(), face);
                assert_eq!(quad.material(), MATERIAL);
                assert_eq!(quad.ao(), [3; 4]);
                assert_eq!(QuadDescriptor::from_le_bytes(quad.to_le_bytes()), Ok(*quad));
                let normal = face.normal();
                for vertex in 0..4 {
                    let position = quad.vertex(vertex);
                    for axis in 0..3 {
                        assert!((0..=extent[axis]).contains(&position[axis]));
                        if normal[axis] != 0 {
                            assert_eq!(
                                position[axis],
                                if normal[axis] < 0 { 0 } else { extent[axis] }
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(
            model.meshes()[BodyPart::Head as usize].opaque[Face::South as usize].tint(),
            SKIN
        );
        assert_eq!(
            model.meshes()[BodyPart::Head as usize].opaque[Face::North as usize].tint(),
            HAIR
        );
        assert_ne!(SHIRT, TROUSERS);
    }
    #[test]
    fn standing_and_crouching_heights_match_the_simulation_body() {
        let model = SelfModel::new();
        let mut player = Player::new([10.0, 64.0, -4.0]);
        let standing = bounds(
            &model,
            SelfModel::poses(&player, 0.0, CameraMode::RightShoulder),
        );
        close(standing.0[1], 64.0);
        close(standing.1[1], 65.8);
        close((standing.0[0] + standing.1[0]) * 0.5, 10.0);
        player.crouch = true;
        let crouched = bounds(
            &model,
            SelfModel::poses(&player, 0.0, CameraMode::RightShoulder),
        );
        close(crouched.0[1], 64.0);
        close(crouched.1[1], 65.5);
    }
    #[test]
    fn feet_interpolate_and_head_rotation_keeps_the_neck_pivot_fixed() {
        let mut player = Player::new([4.0, 12.0, -8.0]);
        player.prev = [2.0, 10.0, -4.0];
        player.yaw = 90.0;
        player.pitch = 30.0;
        let pose = SelfModel::poses(&player, 0.5, CameraMode::RightShoulder);
        let neck = point(pose.transforms[BodyPart::Head as usize], [4.0, 0.0, 4.0]);
        for (actual, expected) in neck.into_iter().zip([3.0, 12.35, -6.0]) {
            close(actual, expected);
        }
        let head = pose.transforms[BodyPart::Head as usize];
        close(head[2][0] / PIXEL, -libm::cosf(std::f32::consts::PI / 6.0));
        close(head[2][1] / PIXEL, -0.5);
        close(head[2][2] / PIXEL, 0.0);
        let first = SelfModel::poses(&player, -5.0, CameraMode::RightShoulder);
        let last = SelfModel::poses(&player, 5.0, CameraMode::RightShoulder);
        close(point(first.transforms[1], [4.0, 0.0, 2.0])[0], 2.0);
        close(point(last.transforms[1], [4.0, 0.0, 2.0])[0], 4.0);
    }
    struct Air;
    impl sim::QueryWorld for Air {
        fn cell(&self, _: [i32; 3]) -> world::CellRead {
            world::CellRead::Known(core_types::Cell::AIR)
        }
    }
    #[test]
    fn simulation_gait_moves_opposite_limbs_and_preserves_attachment_points() {
        let mut player = Player::new([0.0; 3]);
        player.set_flight(true).unwrap();
        for _ in 0..12 {
            player.tick(
                &Air,
                sim::Keys {
                    fwd: true,
                    ..Default::default()
                },
            );
        }
        let pose = SelfModel::poses(&player, 1.0, CameraMode::RightShoulder);
        let left = pose.transforms[BodyPart::LeftLeg as usize];
        let right = pose.transforms[BodyPart::RightLeg as usize];
        assert!(left[1][2].abs() > 1e-4);
        close(left[1][2], -right[1][2]);
        for (part, x) in [
            (BodyPart::LeftArm, 5.5 * PIXEL),
            (BodyPart::RightArm, -5.5 * PIXEL),
        ] {
            let shoulder = point(pose.transforms[part as usize], [1.5, 12.0, 2.0]);
            close(shoulder[0], player.pos[0] as f32 + x);
            close(shoulder[1], player.pos[1] as f32 + 24.0 * PIXEL);
            close(shoulder[2], player.pos[2] as f32);
        }
    }
    #[test]
    fn attack_changes_only_the_right_arm_and_interpolates_wrapped_swing() {
        let mut player = Player::new([0.0; 3]);
        let idle = SelfModel::poses(&player, 0.5, CameraMode::RightShoulder);
        player.last_swing = 0.4;
        player.swing = 0.6;
        let attack = SelfModel::poses(&player, 0.5, CameraMode::RightShoulder);
        assert_ne!(
            attack.transforms[BodyPart::RightArm as usize],
            idle.transforms[BodyPart::RightArm as usize]
        );
        for part in [
            BodyPart::Head,
            BodyPart::Torso,
            BodyPart::LeftArm,
            BodyPart::LeftLeg,
            BodyPart::RightLeg,
        ] {
            assert_eq!(
                attack.transforms[part as usize],
                idle.transforms[part as usize]
            );
        }
        player.last_swing = 0.9;
        player.swing = 0.1;
        let wrapped = SelfModel::poses(&player, 0.5, CameraMode::RightShoulder);
        for (actual, expected) in wrapped.transforms[BodyPart::RightArm as usize]
            .iter()
            .flatten()
            .zip(
                idle.transforms[BodyPart::RightArm as usize]
                    .iter()
                    .flatten(),
            )
        {
            close(*actual, *expected);
        }
    }
    #[test]
    fn visibility_and_invalid_inputs_cannot_send_nonfinite_gpu_matrices() {
        let mut player = Player::new([0.0; 3]);
        assert!(!SelfModel::poses(&player, 0.5, CameraMode::FirstPerson).visible);
        assert!(SelfModel::poses(&player, 0.5, CameraMode::RightShoulder).visible);
        player.mode = GameMode::Spectator;
        assert!(!SelfModel::poses(&player, 0.5, CameraMode::RightShoulder).visible);
        player.pos[0] = f64::NAN;
        let invalid = SelfModel::poses(&player, f64::NAN, CameraMode::RightShoulder);
        assert!(!invalid.visible);
        assert!(
            invalid
                .transforms
                .iter()
                .flatten()
                .flatten()
                .all(|value| value.is_finite())
        );
    }
    #[test]
    fn repeated_pose_updates_allocate_nothing_and_leave_geometry_immutable() {
        let model = SelfModel::new();
        let before: Vec<_> = model
            .meshes()
            .iter()
            .flat_map(|mesh| mesh.iter().map(|q| q.words))
            .collect();
        let mut player = Player::new([3.0, 64.0, 7.0]);
        black_box(SelfModel::poses(&player, 0.5, CameraMode::RightShoulder));
        let control = telemetry::AllocationScope::begin().unwrap();
        black_box(vec![0_u8; 1024]);
        assert!(control.finish().allocating_calls() > 0);
        let scope = telemetry::AllocationScope::begin().unwrap();
        for frame in 0..10_000 {
            player.yaw = frame as f32 * 0.1;
            player.pitch = (frame % 180) as f32 - 90.0;
            black_box(SelfModel::poses(
                &player,
                f64::from(frame % 100) / 100.0,
                CameraMode::RightShoulder,
            ));
        }
        let measured = scope.finish();
        assert_eq!(measured.allocating_calls(), 0);
        assert_eq!(measured.dealloc_calls, 0);
        let after: Vec<_> = model
            .meshes()
            .iter()
            .flat_map(|mesh| mesh.iter().map(|q| q.words))
            .collect();
        assert_eq!(before, after);
    }
}
