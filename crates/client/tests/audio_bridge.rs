use client::audio_bridge::{self, BlockSound};
use core_types::Cell;
use std::cell::Cell as Counter;
use world::CellRead;
#[test]
fn footsteps_use_actual_support_and_unknown_cells_stay_silent() {
    let mut player = sim::Player::new([0.5, 64.0, 0.5]);
    player.prev = [0.2, 64.0, 0.5];
    player.support = Some([1, 63, 0]);
    let query = |at: [i32; 3]| match at {
        [0 | 1, 63, 0] => CellRead::Known(Cell::from_parts(rules::ids::STONE, 0)),
        [1, 64, 0] => CellRead::Known(Cell::from_parts(rules::ids::SNOW_LAYER, 0)),
        _ => CellRead::Known(Cell::AIR),
    };
    let body = audio_bridge::step_body(&player, &query);
    assert_eq!(body.support, audio::events::Surface::Stone);
    assert!(body.snow_above_support && !body.stepping_air && !body.silent);
    player.sneak_in = true;
    assert!(audio_bridge::step_body(&player, &query).silent);
    let unknown = audio_bridge::step_body(&player, &|_| CellRead::Unknown);
    assert!(unknown.stepping_air);
    assert_eq!(unknown.support, audio::events::Surface::None);
}
#[test]
fn cave_probe_has_a_fixed_read_budget_and_never_invents_unknown_walls() {
    let reads = Counter::new(0);
    let open = |_: [i32; 3]| {
        reads.set(reads.get() + 1);
        CellRead::Known(Cell::AIR)
    };
    let cavity = audio_bridge::probe_cavity([0.5; 3], &open);
    assert_eq!(reads.get(), 96);
    assert_eq!(cavity.wall_distances, [None; 6]);
    let unknown = audio_bridge::probe_cavity([0.5; 3], &|_| CellRead::Unknown);
    assert_eq!(unknown.wall_distances, [None; 6]);
    let closed = audio_bridge::probe_cavity([0.5; 3], &|_| {
        CellRead::Known(Cell::from_parts(rules::ids::STONE, 0))
    });
    assert_eq!(closed.wall_distances, [Some(0.5); 6]);
    assert!(
        closed
            .impulse(48000)
            .unwrap()
            .iter()
            .any(|sample| *sample > 0.0)
    );
    assert_eq!(
        audio_bridge::block_event(Cell::from_parts(rules::ids::STONE, 0), BlockSound::Hit),
        Some(("block.stone.step", 0.25))
    );
    assert!(audio_bridge::block_event(Cell::AIR, BlockSound::Break).is_none());
}
