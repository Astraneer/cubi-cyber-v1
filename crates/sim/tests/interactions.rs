use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use sim::{
    EditAction, EditError, GameMode, InteractionController, Mining, Player, RayResult,
    prepare_edit, raycast,
};
use world::CellRead;
fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn scene() -> world::World {
    let mut world = world::World::new(rules::properties, 128);
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    for z in -1..=1 {
        for x in -1..=1 {
            for y in -1..=1 {
                tx.create_page(
                    PageId {
                        space: SpaceId(0),
                        x,
                        y,
                        z,
                    },
                    vec![Cell::AIR; 4096],
                );
            }
        }
    }
    world.commit(tx).unwrap();
    world
}
fn write(world: &mut world::World, p: [i32; 3], cell: Cell) {
    let mut tx = world::Transaction::new(world.tick(), SpaceId(0));
    tx.set_cell(pos(p[0], p[1], p[2]), cell);
    world.commit(tx).unwrap();
}
fn stone() -> Cell {
    Cell::from_parts(3, 0)
}
#[test]
fn dda_straight_axes_at_integer_origins_and_faces() {
    for (direction, block, face) in [
        ([1.0, 0.0, 0.0], [3, 1, 0], 4),
        ([-1.0, 0.0, 0.0], [-3, 1, 0], 5),
        ([0.0, 0.0, 1.0], [0, 1, 3], 2),
        ([0.0, 0.0, -1.0], [0, 1, -3], 3),
    ] {
        let query = |p: [i32; 3]| CellRead::Known(if p == block { stone() } else { Cell::AIR });
        let RayResult::Hit(hit) = raycast(&query, [0.0, 1.5, 0.0], direction, 4.5) else {
            panic!("missed{direction:?}");
        };
        assert_eq!(hit.cell, block);
        assert_eq!(hit.face, face);
    }
}
#[test]
fn ray_origin_inside_and_boundary_moving_away_use_java_faces() {
    let query = |p: [i32; 3]| CellRead::Known(if p == [0, 0, 0] { stone() } else { Cell::AIR });
    let RayResult::Hit(hit) = raycast(&query, [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 4.5) else {
        panic!("inside miss")
    };
    assert_eq!(hit.face, 4);
    assert!((hit.point[0] - 0.5045).abs() < 1e-9);
    assert_eq!(
        raycast(&query, [1.0, 0.5, 0.5], [1.0, 0.0, 0.0], 4.5),
        RayResult::Miss
    );
}
#[test]
fn rays_reject_invalid_vectors_and_unknown_is_not_air() {
    let air = |_p| CellRead::Known(Cell::AIR);
    for direction in [[f64::MAX; 3], [f64::NAN, 0.0, 1.0], [0.0; 3]] {
        assert_eq!(
            raycast(&air, [0.5, 1.5, 0.5], direction, 4.5),
            RayResult::Miss
        );
    }
    let query = |p: [i32; 3]| {
        if p[2] == 1 {
            CellRead::Unknown
        } else {
            CellRead::Known(Cell::AIR)
        }
    };
    assert!(matches!(
        raycast(&query, [0.5, 1.5, 0.5], [0.0, 0.0, 1.0], 4.5),
        RayResult::Unknown([0, 1, 1])
    ));
    let boundary = |p: [i32; 3]| CellRead::Known(if p[2] == 5 { stone() } else { Cell::AIR });
    assert_eq!(
        raycast(&boundary, [0.5, 1.5, 0.5], [0.0, 0.0, 1.0], 4.5),
        RayResult::Miss
    );
}
#[test]
fn hand_mining_uses_independent_oak_and_stone_reference_counts() {
    let mut mining = Mining::default();
    let mut ticks = 0;
    loop {
        ticks += 1;
        if mining.tick_cell([0, 1, 2], Cell::from_parts(9, 0), 0, true, false, false) {
            break;
        }
        assert!(ticks < 100);
    }
    assert_eq!(ticks, 61);
    let mut mining = Mining::default();
    let mut ticks = 0;
    loop {
        ticks += 1;
        if mining.tick_cell([0, 1, 2], stone(), 0, true, false, false) {
            break;
        }
        assert!(ticks < 200);
    }
    assert_eq!(ticks, 151);
}
#[test]
fn survival_controller_limits_progress_to_ticks_and_resets_when_tool_or_block_changes() {
    let mut world = scene();
    write(&mut world, [0, 1, 3], Cell::from_parts(9, 0));
    let player = Player::new([0.5, 0.0, 0.5]);
    let mut controller = InteractionController::default();
    assert!(
        controller
            .tick_break(&world, SpaceId(0), &player, Tick(1), 0, 0, true, |_| false)
            .unwrap()
            .is_none()
    );
    let before = controller.mining.progress;
    assert!(matches!(
        controller.tick_break(&world, SpaceId(0), &player, Tick(1), 0, 0, true, |_| false),
        Err(EditError::DuplicateTick)
    ));
    assert_eq!(before, controller.mining.progress);
    for tick in 2..20 {
        controller
            .tick_break(&world, SpaceId(0), &player, Tick(tick), 0, 0, true, |_| {
                false
            })
            .unwrap();
    }
    assert!(controller.mining.progress > 0.3);
    controller
        .tick_break(&world, SpaceId(0), &player, Tick(20), 0, 1, true, |_| false)
        .unwrap();
    assert!(controller.mining.progress < 0.02);
    write(&mut world, [0, 1, 3], stone());
    controller
        .tick_break(&world, SpaceId(0), &player, Tick(21), 0, 1, true, |_| false)
        .unwrap();
    assert!(controller.mining.progress < 0.007);
}
#[test]
fn completed_mining_prepares_one_guarded_world_transaction() {
    let mut world = scene();
    write(&mut world, [0, 1, 3], Cell::from_parts(9, 0));
    let player = Player::new([0.5, 0.0, 0.5]);
    let mut controller = InteractionController::default();
    for tick in 1..=61 {
        let result = controller
            .tick_break(&world, SpaceId(0), &player, Tick(tick), 0, 0, true, |_| {
                false
            })
            .unwrap();
        if tick < 61 {
            assert!(result.is_none());
        } else {
            world.commit(result.expect("oak hand61ticks")).unwrap();
        }
    }
    assert_eq!(world.read(pos(0, 1, 3)), CellRead::Known(Cell::AIR));
}
#[test]
fn edit_permissions_visibility_reach_and_stale_cells_are_validated() {
    let mut world = scene();
    write(&mut world, [0, 1, 3], stone());
    let mut player = Player::new([0.5, 0.0, 0.5]);
    let action = EditAction::Break {
        position: [0, 1, 3],
        expected: stone(),
    };
    assert!(matches!(
        prepare_edit(&world, SpaceId(0), &player, action, Tick(1), |_| true),
        Err(EditError::Protected)
    ));
    player.set_mode(GameMode::Spectator);
    assert!(matches!(
        prepare_edit(&world, SpaceId(0), &player, action, Tick(1), |_| false),
        Err(EditError::Spectator)
    ));
    player.set_mode(GameMode::Survival);
    let stale = EditAction::Break {
        position: [0, 1, 3],
        expected: Cell::AIR,
    };
    assert!(matches!(
        prepare_edit(&world, SpaceId(0), &player, stale, Tick(1), |_| false),
        Err(EditError::Stale)
    ));
    write(&mut world, [0, 1, 1], stone());
    assert!(matches!(
        prepare_edit(&world, SpaceId(0), &player, action, Tick(1), |_| false),
        Err(EditError::Occluded)
    ));
}
#[test]
fn creative_can_break_bedrock_but_survival_cannot() {
    let mut world = scene();
    let bedrock = Cell::from_parts(22, 0);
    write(&mut world, [0, 1, 3], bedrock);
    let mut player = Player::new([0.5, 0.0, 0.5]);
    let action = EditAction::Break {
        position: [0, 1, 3],
        expected: bedrock,
    };
    assert!(matches!(
        prepare_edit(&world, SpaceId(0), &player, action, Tick(1), |_| false),
        Err(EditError::Unbreakable)
    ));
    player.set_mode(GameMode::Creative);
    let mut controller = InteractionController::default();
    let transaction = controller
        .tick_break(&world, SpaceId(0), &player, Tick(1), 0, 0, true, |_| false)
        .unwrap()
        .expect("creative bedrock is immediate");
    world.commit(transaction).unwrap();
    assert_eq!(world.read(pos(0, 1, 3)), CellRead::Known(Cell::AIR));
}
#[test]
fn placement_rejects_body_overlap_and_full_snow_replacement() {
    let mut world = scene();
    write(&mut world, [0, 1, 1], stone());
    let player = Player::new([0.5, 0.0, 0.5]);
    let overlap = EditAction::Place {
        position: [0, 1, 0],
        expected: Cell::AIR,
        cell: stone(),
    };
    assert!(matches!(
        prepare_edit(&world, SpaceId(0), &player, overlap, Tick(1), |_| false),
        Err(EditError::Occupied)
    ));
    let snow = Cell::from_parts(54, 7);
    write(&mut world, [0, 1, 3], snow);
    let replace = EditAction::Place {
        position: [0, 1, 3],
        expected: snow,
        cell: stone(),
    };
    assert!(matches!(
        prepare_edit(&world, SpaceId(0), &player, replace, Tick(1), |_| false),
        Err(EditError::Occupied)
    ));
}
#[test]
fn placement_derives_log_axis_and_ladder_support() {
    let mut world = scene();
    write(&mut world, [0, 1, 3], stone());
    let player = Player::new([0.5, 0.0, 0.5]);
    let action = EditAction::Place {
        position: [0, 1, 2],
        expected: Cell::AIR,
        cell: Cell::from_parts(9, u16::MAX),
    };
    world
        .commit(prepare_edit(&world, SpaceId(0), &player, action, Tick(2), |_| false).unwrap())
        .unwrap();
    assert_eq!(
        world.read(pos(0, 1, 2)),
        CellRead::Known(Cell::from_parts(9, 2))
    );
    write(&mut world, [0, 1, 2], Cell::AIR);
    let action = EditAction::Place {
        position: [0, 1, 2],
        expected: Cell::AIR,
        cell: Cell::from_parts(53, 0),
    };
    world
        .commit(prepare_edit(&world, SpaceId(0), &player, action, Tick(3), |_| false).unwrap())
        .unwrap();
    assert_eq!(
        world.read(pos(0, 1, 2)),
        CellRead::Known(Cell::from_parts(53, 2))
    );
}
#[test]
fn held_transaction_cannot_ignore_changed_visibility_pages() {
    let mut world = scene();
    write(&mut world, [0, 1, 18], stone());
    let mut player = Player::new([0.5, 0.0, 14.5]);
    player.set_mode(GameMode::Creative);
    let action = EditAction::Break {
        position: [0, 1, 18],
        expected: stone(),
    };
    let tx = prepare_edit(&world, SpaceId(0), &player, action, Tick(2), |_| false).unwrap();
    // Occluder is on pagez0, targetonpagez1. Only guardingtargetwouldacceptthis.
    write(&mut world, [0, 1, 15], stone());
    assert!(world.commit(tx).is_err());
    assert_eq!(world.read(pos(0, 1, 18)), CellRead::Known(stone()));
}
