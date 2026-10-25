use sim::{Keys, Player, prediction::PredictionState};
use std::{alloc::System, hint::black_box};
use telemetry::{AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);
#[test]
fn capture_validation_and_rollback_reuse_scratch_without_allocations() {
    let mut player = Player::new([1.0, 12.0, 3.0]);
    let state = PredictionState::capture(&player).unwrap();
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        let state = PredictionState::capture(black_box(&player)).unwrap();
        let decoded = PredictionState::decode(black_box(state.as_bytes())).unwrap();
        decoded.apply_authoritative(black_box(&mut player)).unwrap();
        black_box(decoded.hash());
        assert!(PredictionState::decode(&[0; 384]).is_err());
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    assert_eq!(PredictionState::capture(&player).unwrap(), state);
    let scope = AllocationScope::begin().unwrap();
    let allocated = black_box(Player::new([0.0; 3]));
    drop(allocated);
    let positive = scope.finish();
    assert!(positive.allocating_calls() >= 3);
    assert!(positive.dealloc_calls >= 3);
}
#[test]
fn retained_scratch_has_the_same_next_physics_ticks_as_new_scratch() {
    let world = |p: [i32; 3]| {
        world::CellRead::Known(core_types::Cell::from_parts(
            if p[1] < 0 {
                rules::ids::STONE
            } else {
                rules::ids::AIR
            },
            0,
        ))
    };
    let mut original = Player::new([1.5, 8.0, 1.5]);
    let mut reused = Player::new([0.0, 0.0, 0.0]);
    for tick in 0..500 {
        let state = PredictionState::capture(&original).unwrap();
        state.apply_authoritative(&mut reused).unwrap();
        let mut fresh = state.to_player().unwrap();
        let keys = Keys {
            fwd: true,
            jump: tick % 31 == 0,
            sprint: tick % 9 == 0,
            sneak: tick % 13 == 0,
            ..Keys::default()
        };
        let a = original.tick(&world, keys);
        assert_eq!(a, reused.tick(&world, keys));
        assert_eq!(a, fresh.tick(&world, keys));
        assert_eq!(
            PredictionState::capture(&original).unwrap(),
            PredictionState::capture(&reused).unwrap()
        );
        assert_eq!(
            PredictionState::capture(&original).unwrap(),
            PredictionState::capture(&fresh).unwrap()
        );
    }
}
