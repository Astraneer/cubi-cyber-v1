//! Independent numerical references from Java1.21.1 / allowlisted donor golden tests.
use core_types::{Cell, PageId, SpaceId, Tick};
use sim::collision::Solids;
use sim::{Aabb, CameraMode, Keys, Permissions, Player};
use world::CellRead;

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn bx(lo: [f64; 3], hi: [f64; 3]) -> Aabb {
    Aabb { lo, hi }
}
struct Boxes {
    boxes: Vec<Aabb>,
}
impl Boxes {
    fn flat(extra: &[Aabb]) -> Self {
        let mut boxes = vec![bx([-10_000.0, -64.0, -10_000.0], [10_000.0, 0.0, 10_000.0])];
        boxes.extend_from_slice(extra);
        Self { boxes }
    }
}
impl Solids for Boxes {
    fn shapes_at(&mut self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        let lo = [f64::from(x), f64::from(y), f64::from(z)];
        let hi = lo.map(|v| v + 1.0);
        for shape in &self.boxes {
            let clipped = bx(
                std::array::from_fn(|a| shape.lo[a].max(lo[a])),
                std::array::from_fn(|a| shape.hi[a].min(hi[a])),
            );
            if (0..3).all(|a| clipped.lo[a] < clipped.hi[a]) {
                out.push(clipped);
            }
        }
    }
    fn slip(&mut self, _: i32, _: i32, _: i32) -> f32 {
        0.6
    }
    fn chokes(&mut self, _: i32, _: i32, _: i32) -> bool {
        false
    }
}
fn forward() -> Keys {
    Keys {
        fwd: true,
        ..Keys::default()
    }
}
fn steady(keys: Keys, sprint: bool) -> f64 {
    let mut world = Boxes::flat(&[]);
    let mut player = Player::new([0.5, 0.0, 0.5]);
    player.sprinting = sprint;
    player.sneak_in = keys.sneak;
    for _ in 0..300 {
        player.tick_solids(&mut world, keys);
    }
    let before = player.pos;
    for _ in 0..100 {
        player.tick_solids(&mut world, keys);
    }
    ((player.pos[0] - before[0]).powi(2) + (player.pos[2] - before[2]).powi(2)).sqrt() / 100.0
        * 20.0
}
#[test]
fn exact_land_steady_speeds() {
    for (keys, sprint, want) in [
        (forward(), false, 4.31718),
        (
            Keys {
                sprint: true,
                ..forward()
            },
            true,
            5.61234,
        ),
        (
            Keys {
                left: true,
                ..forward()
            },
            false,
            4.40529,
        ),
        (
            Keys {
                sneak: true,
                ..forward()
            },
            false,
            1.29515,
        ),
        (
            Keys {
                sneak: true,
                left: true,
                ..forward()
            },
            false,
            1.83163,
        ),
        (
            Keys {
                sneak: true,
                sprint: true,
                ..forward()
            },
            true,
            1.68370,
        ),
    ] {
        let got = steady(keys, sprint);
        assert!((got - want).abs() < 1e-5, "{keys:?}: {got} != {want}");
    }
}
#[test]
fn acceleration_braking_and_sprint_jump_references() {
    let mut world = Boxes::flat(&[]);
    let mut player = Player::new([0.5, 0.0, 0.5]);
    for want in [
        0.0980, 0.1515, 0.1807, 0.1967, 0.2054, 0.2101, 0.2127, 0.2142, 0.2149, 0.2154,
    ] {
        let before = player.pos[2];
        player.tick_solids(&mut world, forward());
        assert!((player.pos[2] - before - want).abs() < 1e-4);
    }
    for (sprint, ticks, brake) in [(false, 48, 0.2558), (true, 37, 0.3326)] {
        let mut player = Player::new([0.5, 0.0, 0.5]);
        let keys = Keys {
            sprint,
            ..forward()
        };
        let mut count = 0;
        while player.pos[2] - 0.5 < 10.0 {
            player.tick_solids(&mut world, keys);
            count += 1;
            assert!(count < 100);
        }
        assert_eq!(count, ticks);
        for _ in 0..200 {
            player.tick_solids(&mut world, keys);
        }
        let before = player.pos[2];
        for _ in 0..20 {
            player.tick_solids(&mut world, Keys::default());
        }
        assert!((player.pos[2] - before - brake).abs() < 1e-4);
    }
    let mut player = Player::new([0.5, 0.0, 0.5]);
    let keys = Keys {
        sprint: true,
        ..forward()
    };
    for _ in 0..100 {
        player.tick_solids(&mut world, keys);
    }
    let before = player.pos[2];
    for _ in 0..600 {
        player.tick_solids(&mut world, Keys { jump: true, ..keys });
    }
    assert!(((player.pos[2] - before) / 600.0 * 20.0 - 7.1).abs() < 1e-3);
}
#[test]
fn jump_height_and_twelve_tick_period() {
    let mut world = Boxes::flat(&[]);
    let mut player = Player::new([0.5, 0.0, 0.5]);
    player.tick_solids(&mut world, Keys::default());
    let reference = [
        0.4200, 0.7532, 1.0013, 1.1661, 1.2492, 1.2522, 1.1768, 1.0244, 0.7967, 0.4952, 0.1213, 0.0,
    ];
    let mut max = 0.0f64;
    for (index, want) in reference.into_iter().enumerate() {
        player.tick_solids(
            &mut world,
            Keys {
                jump: index == 0,
                ..Keys::default()
            },
        );
        max = max.max(player.pos[1]);
        assert!(
            (player.pos[1] - want).abs() < 1e-4,
            "tick{index}: {:?}",
            player.pos
        );
    }
    assert!(player.on_ground);
    assert!((max - 1.252_203).abs() < 1e-6);
    let mut times = Vec::new();
    for t in 0..80 {
        let before = player.pos[1];
        player.tick_solids(
            &mut world,
            Keys {
                jump: true,
                ..Keys::default()
            },
        );
        if before == 0.0 && player.pos[1] > 0.15 {
            times.push(t);
        }
    }
    assert!(times.windows(2).all(|v| v[1] - v[0] == 12));
}
#[test]
fn jump_cooldown_under_two_block_ceiling() {
    let mut world = Boxes::flat(&[bx([-100.0, 2.0, -100.0], [100.0, 3.0, 100.0])]);
    let mut player = Player::new([0.5, 0.0, 0.5]);
    let mut times = Vec::new();
    for t in 0..80 {
        let before = player.pos[1];
        player.tick_solids(
            &mut world,
            Keys {
                jump: true,
                ..Keys::default()
            },
        );
        if before == 0.0 && player.pos[1] > 0.15 {
            times.push(t);
        }
    }
    assert!(times.len() > 5);
    assert!(times.windows(2).all(|v| v[1] - v[0] == 10));
}
#[test]
fn crouch_ledge_and_diagonal_corner_references() {
    let mut world = Boxes {
        boxes: vec![bx([-100.0, -1.0, -100.0], [100.0, 0.0, 0.0])],
    };
    let mut player = Player::new([0.5, 0.0, -2.0]);
    player.sneak_in = true;
    let keys = Keys {
        sneak: true,
        ..forward()
    };
    for _ in 0..200 {
        player.tick_solids(&mut world, keys);
    }
    assert_eq!(player.pos[1], 0.0);
    assert!((player.pos[2] - 0.2977).abs() < 1e-4);
    let mut world = Boxes {
        boxes: vec![bx([-100.0, -1.0, -100.0], [0.0, 0.0, 0.0])],
    };
    let mut player = Player::new([-2.0, 0.0, -2.0]);
    player.yaw = -45.0;
    player.sneak_in = true;
    for _ in 0..300 {
        player.tick_solids(&mut world, keys);
    }
    assert_eq!(player.pos[1], 0.0);
    assert!((player.pos[0] - 0.2803).abs() < 1e-4 && (player.pos[2] - 0.2803).abs() < 1e-4);
}
#[test]
fn step_height_boundary_in_full_player_motion() {
    for (height, up) in [(0.5, true), (0.6, true), (0.625, false), (1.0, false)] {
        let mut world = Boxes::flat(&[bx([-2.0, 0.0, 0.0], [2.0, height, 4.0])]);
        let mut player = Player::new([0.5, 0.0, -1.0]);
        for _ in 0..10 {
            player.tick_solids(&mut world, forward());
        }
        assert_eq!(
            player.pos[1] > 0.4,
            up,
            "height{height} position{:?}",
            player.pos
        );
    }
}
#[test]
fn cliff_fall_distances_and_health() {
    for (height, air, want, damage) in [
        (1.0, 5, 0.7685, 0),
        (2.0, 7, 1.5926, 0),
        (3.0, 9, 2.6946, 0),
        (4.0, 10, 3.3463, 1),
        (10.0, 17, 9.6671, 7),
        (23.0, 26, 21.8335, 19),
    ] {
        let mut world = Boxes {
            boxes: vec![
                bx([-100.0, height - 1.0, -100.0], [100.0, height, 0.0]),
                bx([-100.0, -64.0, -100.0], [100.0, 0.0, 100.0]),
            ],
        };
        let mut player = Player::new([0.5, height, -1.0]);
        for _ in 0..3 {
            player.tick_solids(&mut world, forward());
        }
        let mut left = None;
        for tick in 1..100 {
            player.tick_solids(&mut world, forward());
            if left.is_none() && !player.on_ground {
                left = Some(tick);
            }
            if left.is_some() && player.on_ground {
                assert_eq!(tick - left.unwrap() + 1, air);
                break;
            }
        }
        assert!((f64::from(player.landed.unwrap().1) - want).abs() < 1e-4);
        assert_eq!(player.health, 20.0 - damage as f32);
    }
    for (distance, want) in [(3.0, 0), (3.000_000_2, 1), (4.0, 1), (23.0, 20)] {
        assert_eq!(sim::fall_damage(distance, 1.0), want);
    }
}
#[test]
fn survival_flight_numbers_and_landing() {
    let mut world = Boxes::flat(&[]);
    let mut player = Player::new([0.5, 0.0, 0.5]);
    player.tick_solids(&mut world, Keys::default());
    player.set_flight(true).unwrap();
    for _ in 0..5 {
        player.tick_solids(&mut world, Keys::default());
    }
    assert!(player.flying && !player.on_ground);
    assert_eq!(player.pos[1], 0.0);
    for _ in 0..40 {
        player.tick_solids(
            &mut world,
            Keys {
                jump: true,
                ..Keys::default()
            },
        );
    }
    assert!((player.pos[1] - player.prev[1] - 0.375).abs() < 1e-6);
    for _ in 0..80 {
        player.tick_solids(&mut world, forward());
    }
    assert!((player.pos[2] - player.prev[2] - 0.5444).abs() < 1e-3);
    assert!((player.pos[1] - player.prev[1]).abs() < 1e-9);
    for _ in 0..80 {
        player.tick_solids(
            &mut world,
            Keys {
                sprint: true,
                ..forward()
            },
        );
    }
    assert!((player.pos[2] - player.prev[2] - 1.0889).abs() < 2e-3);
    for _ in 0..200 {
        if !player.flying {
            break;
        }
        player.tick_solids(
            &mut world,
            Keys {
                sneak: true,
                ..Keys::default()
            },
        );
    }
    assert!(!player.flying && player.on_ground);
    assert_eq!(player.health, 20.0);
}
#[test]
fn flight_collision_debug_gate_and_damage_after_disable() {
    let mut world = Boxes::flat(&[bx([-10.0, 0.0, 3.0], [10.0, 10.0, 4.0])]);
    let mut player = Player::new([0.5, 2.0, 0.5]);
    player.on_ground = false;
    player.set_flight(true).unwrap();
    for _ in 0..80 {
        player.tick_solids(&mut world, forward());
    }
    assert!((player.pos[2] - 2.7).abs() < 1e-6);
    assert!(
        player
            .set_debug_noclip(
                true,
                Permissions {
                    server_debug: false
                }
            )
            .is_err()
    );
    assert!(!player.noclip);
    player
        .set_debug_noclip(true, Permissions { server_debug: true })
        .unwrap();
    for _ in 0..10 {
        player.tick_solids(&mut world, forward());
    }
    assert!(player.pos[2] > 4.0);
    player.revoke_debug_permission();
    assert!(!player.noclip && !player.flying);
    let mut world = Boxes::flat(&[]);
    let mut player = Player::new([0.5, 10.6, 0.5]);
    player.on_ground = false;
    player.vel = [0.0; 3];
    player.set_flight(true).unwrap();
    for _ in 0..20 {
        player.tick_solids(&mut world, Keys::default());
    }
    assert_eq!(player.health, 20.0);
    player.set_flight(false).unwrap();
    for _ in 0..80 {
        player.tick_solids(&mut world, Keys::default());
        if player.on_ground {
            break;
        }
    }
    assert!(player.on_ground);
    assert!(player.health <= 13.0);
}
fn water_world(position: [i32; 3]) -> CellRead {
    CellRead::Known(Cell::from_parts(
        if (0..200).contains(&position[1]) {
            15
        } else {
            0
        },
        0,
    ))
}
#[test]
fn swimming_sinking_and_crouching_water_references() {
    for (keys, values) in [
        (
            Keys::default(),
            vec![0.0, -0.005, -0.009, -0.0122, -0.01476],
        ),
        (
            Keys {
                jump: true,
                ..Keys::default()
            },
            vec![0.04, 0.067, 0.0886, 0.10588, 0.119704],
        ),
        (
            Keys {
                sneak: true,
                ..Keys::default()
            },
            vec![-0.04, -0.077, -0.1066, -0.13028],
        ),
    ] {
        let mut player = Player::new([0.5, 50.0, 0.5]);
        player.vel = [0.0; 3];
        player.on_ground = false;
        for want in values {
            let before = player.pos[1];
            player.tick(&water_world, keys);
            assert!(
                (player.pos[1] - before - want).abs() < 2e-6,
                "{keys:?}:{} !={want}",
                player.pos[1] - before
            );
        }
    }
    let mut player = Player::new([0.5, 50.0, 0.5]);
    player.vel = [0.0; 3];
    player.on_ground = false;
    for (tick, want) in [0.0196, 0.03528, 0.047824].into_iter().enumerate() {
        let before = player.pos[2];
        player.tick(&water_world, forward());
        assert!((player.pos[2] - before - want).abs() < 1e-5, "tick{tick}");
    }
    for _ in 0..40 {
        player.tick(
            &water_world,
            Keys {
                sprint: true,
                ..forward()
            },
        );
    }
    assert!(!player.sprinting);
    assert!((player.pos[2] - player.prev[2] - 0.098).abs() < 1e-3);
}
#[test]
fn ladder_climb_slide_and_hold_references() {
    let world = |p: [i32; 3]| {
        CellRead::Known(Cell::from_parts(
            if p[1] < 0 || p[2] == 1 {
                3
            } else if p[2] == 0 && p[0] == 0 {
                53
            } else {
                0
            },
            if p[2] == 0 && p[0] == 0 && p[1] >= 0 {
                2
            } else {
                0
            },
        ))
    };
    let mut player = Player::new([0.5, 0.0, 0.5]);
    for _ in 0..30 {
        player.tick(&world, forward());
    }
    assert!((player.pos[1] - player.prev[1] - 0.1176).abs() < 1e-6);
    assert_eq!(player.fall, 0.0);
    for _ in 0..6 {
        player.tick(&world, Keys::default());
    }
    assert!((player.pos[1] - player.prev[1] + 0.15).abs() < 1e-6);
    for _ in 0..2 {
        player.tick(
            &world,
            Keys {
                sneak: true,
                ..Keys::default()
            },
        );
    }
    let before = player.pos[1];
    for _ in 0..10 {
        player.tick(
            &world,
            Keys {
                sneak: true,
                ..Keys::default()
            },
        );
    }
    assert_eq!(player.pos[1], before);
    player.tick(
        &world,
        Keys {
            jump: true,
            ..Keys::default()
        },
    );
    player.tick(
        &world,
        Keys {
            jump: true,
            ..Keys::default()
        },
    );
    assert!(player.pos[1] > before);
}
#[test]
fn unknown_body_freezes_and_unknown_boundary_is_solid() {
    let mut world = world::World::new(rules::properties, 64);
    let mut player = Player::new([0.5, 0.0, 0.5]);
    let hash = player.state_hash();
    let result = player.tick(&sim::WorldQuery::new(&world, SpaceId(0)), forward());
    assert!(result.waiting_for_world);
    assert_eq!(hash, player.state_hash());
    let mut tx = world::Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        vec![Cell::AIR; 4096],
    );
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: -1,
            z: 0,
        },
        vec![Cell::from_parts(3, 0); 4096],
    );
    world.commit(tx).unwrap();
    let mut boundary = false;
    for _ in 0..200 {
        boundary |= player
            .tick(
                &sim::WorldQuery::new(&world, SpaceId(0)),
                Keys {
                    left: true,
                    ..Keys::default()
                },
            )
            .unknown_boundary;
    }
    assert!(boundary);
    assert!(player.pos[0] <= 15.7 + 1e-6);
    assert_eq!(player.health, 20.0);
}
#[test]
fn camera_interpolates_clips_right_shoulder_and_reuses_existing_model_pose() {
    let world = |p: [i32; 3]| CellRead::Known(Cell::from_parts(if p[2] == -2 { 3 } else { 0 }, 0));
    let mut player = Player::new([0.5, 0.0, 0.5]);
    let camera = player.camera(1.0, CameraMode::RightShoulder, &world);
    assert!(camera.self_visible);
    assert!(camera.eye[0] < 0.5);
    assert!(camera.eye[2] >= -0.9 - 1e-6, "{:?}", camera.eye);
    player.prev = [2.0, 0.0, 0.5];
    player.pos = [4.0, 0.0, 0.5];
    let camera = player.camera(0.5, CameraMode::FirstPerson, &world);
    assert!(!camera.self_visible);
    assert_eq!(camera.eye[0], 3.0);
    player.turn(2400.0, 0.0, 0.5);
    assert!((player.yaw - 360.0).abs() < 1e-3);
    player.turn(0.0, 1e6, 0.5);
    assert_eq!(player.pitch, 90.0);
}
#[test]
fn actual_static_tick_and_camera_hot_paths_allocate_nothing() {
    let world = |p: [i32; 3]| CellRead::Known(Cell::from_parts(if p[1] < 0 { 3 } else { 0 }, 0));
    let mut player = Player::new([0.5, 0.0, 0.5]);
    for _ in 0..10 {
        player.tick(&world, Keys::default());
        let _ = player.camera(0.5, CameraMode::RightShoulder, &world);
    }
    let scope = telemetry::AllocationScope::begin().unwrap();
    let control = std::hint::black_box(vec![1u8; 37]);
    std::hint::black_box(&control);
    let counts = scope.finish();
    drop(control);
    assert!(counts.alloc_calls > 0);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        player.tick(&world, Keys::default());
        std::hint::black_box(player.camera(0.5, CameraMode::RightShoulder, &world));
        std::hint::black_box(player.self_body.pose(0.5));
    }
    let counts = scope.finish();
    assert_eq!(
        counts.alloc_calls + counts.zeroed_alloc_calls + counts.realloc_calls,
        0
    );
}
#[test]
fn fixed_tick_state_hash_determinism_catches_input_changes() {
    let world = |p: [i32; 3]| CellRead::Known(Cell::from_parts(if p[1] < 0 { 3 } else { 0 }, 0));
    let mut a = Player::new([0.5, 0.0, 0.5]);
    let mut b = Player::new([0.5, 0.0, 0.5]);
    for tick in 0..240 {
        let input = Keys {
            fwd: tick < 200,
            sprint: tick > 40,
            jump: tick % 19 == 0,
            ..Keys::default()
        };
        a.tick(&world, input);
        b.tick(&world, input);
        assert_eq!(a.state_hash(), b.state_hash(), "tick{tick}");
    }
    b.tick(
        &world,
        Keys {
            left: true,
            ..Keys::default()
        },
    );
    a.tick(&world, Keys::default());
    assert_ne!(a.state_hash(), b.state_hash());
}

#[test]
fn camera_fov_eye_height_and_mouse_golden() {
    let mut world = Boxes::flat(&[]);
    let mut player = Player::new([0.5, 0.0, 0.5]);
    player.sprinting = true;
    player.tick_solids(
        &mut world,
        Keys {
            sprint: true,
            ..forward()
        },
    );
    assert!((player.fov * 70.0 - 75.25).abs() < 1e-3);
    player.tick_solids(
        &mut world,
        Keys {
            sprint: true,
            ..forward()
        },
    );
    assert!((player.fov * 70.0 - 77.875).abs() < 1e-3);
    player.crouch = true;
    player.tick_solids(
        &mut world,
        Keys {
            sneak: true,
            ..Keys::default()
        },
    );
    assert!((player.cam_y - 1.445).abs() < 1e-4);
    player.tick_solids(
        &mut world,
        Keys {
            sneak: true,
            ..Keys::default()
        },
    );
    assert!((player.cam_y - 1.3575).abs() < 1e-4);
}
