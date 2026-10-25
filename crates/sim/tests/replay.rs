use core_types::Cell;
use sim::{
    Keys, Player, QueryWorld,
    prediction::PredictionState,
    replay::{Archive, Recorder, WINDOW_NS},
};
use std::{alloc::System, hint::black_box};
use telemetry::{AllocationScope, CountingAllocator};
use world::CellRead;
#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);
struct Landscape {
    phase: usize,
}
impl QueryWorld for Landscape {
    fn cell(&self, p: [i32; 3]) -> CellRead {
        if self.phase == 3 && p[0] > 4 {
            return CellRead::Unknown;
        }
        CellRead::Known(Cell::from_parts(
            if p[1] < 0 {
                rules::ids::STONE
            } else if self.phase == 2 && p[1] < 2 {
                rules::ids::WATER
            } else if p[0] == 3 && p[1] == 0 && p[2] < 0 {
                rules::ids::STONE
            } else {
                rules::ids::AIR
            },
            0,
        ))
    }
    fn flow_override(&self, _: [i32; 3]) -> Option<[f64; 3]> {
        (self.phase == 2).then_some([0.125, 0.0, -0.25])
    }
}
#[test]
fn actual_movement_replays_flight_liquids_unknown_and_authority_checkpoints() {
    let mut recorder = Recorder::new(512).unwrap();
    let mut player = Player::new([0.5, 3.0, 0.5]);
    let mut plain = Player::new(player.pos);
    for tick in 0..300 {
        recorder.set_time(tick as u64 * 50_000_000).unwrap();
        if tick % 75 == 0 {
            player.teleport([0.5, 1.0, 0.5]).unwrap();
            player.set_flight(tick == 75).unwrap();
        }
        player.yaw = (tick % 31) as f32 * 3.0;
        PredictionState::capture(&player)
            .unwrap()
            .apply_authoritative(&mut plain)
            .unwrap();
        let world = Landscape { phase: tick / 75 };
        let keys = Keys {
            fwd: true,
            sprint: tick % 7 == 0,
            jump: tick % 29 == 0,
            sneak: tick % 11 == 0,
            ..Keys::default()
        };
        let a = recorder.tick(&mut player, &world, keys, tick % 13 == 0);
        assert_eq!(a, plain.tick_using(&world, keys, tick % 13 == 0));
        assert_eq!(
            PredictionState::capture(&player).unwrap(),
            PredictionState::capture(&plain).unwrap()
        );
    }
    let stats = recorder.stats();
    assert_eq!(stats.steps, 201);
    assert_eq!(stats.capacity_losses, 0);
    assert_eq!(stats.invalid_steps, 0);
    assert!(stats.complete_window);
    assert!(stats.flow_witnesses > 0);
    assert!(stats.cell_witnesses > 201);
    let bytes = recorder.encode().unwrap();
    let result = Archive::decode(&bytes).unwrap().replay().unwrap();
    assert_eq!(result.steps, 201);
    assert_eq!(
        result.final_hash,
        PredictionState::capture(&player).unwrap().hash()
    );
    let mut json = "{\"frames\":[]}".to_owned();
    recorder.append_json(&mut json).unwrap();
    let via_json = Archive::from_report(&json).unwrap().replay().unwrap();
    assert_eq!(result, via_json);
    assert!(json.ends_with("\"}}"));
}
#[test]
fn rolling_capture_and_expiry_are_allocation_free_and_report_real_capacity_loss() {
    let mut player = Player::new([0.5, 1.0, 0.5]);
    let mut recorder = Recorder::new(4).unwrap();
    let world = Landscape { phase: 0 };
    recorder.tick(&mut player, &world, Keys::default(), false);
    let scope = AllocationScope::begin().unwrap();
    for tick in 1..10_001u64 {
        recorder.set_time(tick * 50_000_000).unwrap();
        black_box(recorder.tick(&mut player, &world, Keys::default(), false));
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0, "{counts:?}");
    assert_eq!(counts.dealloc_calls, 0);
    let stats = recorder.stats();
    assert_eq!(stats.steps, 4);
    assert!(stats.capacity_losses > 0);
    assert!(!stats.complete_window);
    assert_eq!(
        Archive::decode(&recorder.encode().unwrap())
            .unwrap()
            .replay()
            .unwrap()
            .steps,
        4
    );
    recorder.set_time(500_000_000_000 + WINDOW_NS + 1).unwrap();
    assert_eq!(recorder.stats().steps, 0);
    assert!(recorder.stats().complete_window);
    assert!(recorder.encode().is_err());
    assert!(recorder.set_time(0).is_err());
    let scope = AllocationScope::begin().unwrap();
    drop(black_box(Recorder::new(2).unwrap()));
    assert!(
        scope.finish().allocating_calls() > 0,
        "allocator positive control"
    );
}
#[test]
fn malformed_or_tampered_archives_fail_before_any_network_or_world_access() {
    assert!(Recorder::new(0).is_err());
    assert!(Recorder::new(4097).is_err());
    let mut recorder = Recorder::new(16).unwrap();
    let mut player = Player::new([0.5, 1.0, 0.5]);
    for tick in 0..3 {
        recorder.set_time(tick * 50_000_000).unwrap();
        recorder.tick(
            &mut player,
            &Landscape { phase: 0 },
            Keys {
                fwd: true,
                ..Keys::default()
            },
            false,
        );
    }
    let bytes = recorder.encode().unwrap();
    for len in 0..bytes.len() {
        assert!(Archive::decode(&bytes[..len]).is_err(), "truncation {len}");
    }
    let mut bad = bytes.clone();
    bad.push(0);
    assert!(Archive::decode(&bad).is_err());
    bad = bytes.clone();
    bad[8] = 2;
    assert!(Archive::decode(&bad).is_err());
    bad = bytes.clone();
    bad[26] ^= 1; // first key bit, after seq/time/external
    assert!(
        Archive::decode(&bad).unwrap().replay().is_err(),
        "input corruption is detected by real physics"
    );
    let mut report = "{}".to_owned();
    recorder.append_json(&mut report).unwrap();
    let corrupt = report.replacen("dataHex\":\"4", "dataHex\":\"z", 1);
    assert!(Archive::from_report(&corrupt).is_err());
    assert!(Archive::from_report("{}").is_err());
}
