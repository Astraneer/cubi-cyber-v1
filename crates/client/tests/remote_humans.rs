use client::remote_humans::{Error, RemoteHumans};
use core_types::{SpaceId, Tick};
use net::{PlayerId, PlayerState};
use rules::entities::mob::Instance;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn empty() -> Instance {
    Instance {
        position: [0.; 3],
        body_yaw: 0.,
        head_yaw: 0.,
        pitch: 0.,
        limb_phase: 0.,
        limb_amount: 0.,
        kind: 0,
        flags: 0,
        color: 0,
        id_low: 0,
        id_high: 0,
        death_roll: 0.,
        scale: 1.,
        reserved: 0.,
    }
}
fn player(id: u8) -> PlayerState {
    PlayerState {
        identity: PlayerId([id; 16]),
        space: SpaceId(0),
        health: 20.,
        ..Default::default()
    }
}
#[test]
fn confirmed_human_uses_shared_motion_animation_and_interpolates_wrapped_head_yaw() {
    let mut humans = RemoteHumans::new(64).unwrap();
    let mut output = [empty(); 64];
    let mut state = player(1);
    state.yaw = 179.;
    assert_eq!(
        humans.write(
            Tick(10),
            10.,
            [&state].into_iter(),
            PlayerId::default(),
            SpaceId(0),
            [0; 3],
            &mut output
        ),
        Ok(1)
    );
    assert_eq!(output[0].kind, 5);
    assert_eq!(output[0].body_yaw, 179.);
    state.position[0] = 0.4;
    state.yaw = -179.;
    humans
        .write(
            Tick(12),
            11.,
            [&state].into_iter(),
            PlayerId::default(),
            SpaceId(0),
            [0; 3],
            &mut output,
        )
        .unwrap();
    assert!((output[0].position[0] - 0.2).abs() < 1e-6);
    assert!((output[0].head_yaw - 180.).abs() < 1e-6);
    // Two0.2m motion ticks: amount0.32 then0.512, accumulated phase0.832.
    assert!((output[0].limb_phase - 0.416).abs() < 1e-6);
    assert!((output[0].limb_amount - 0.256).abs() < 1e-6);
    assert_eq!(output[0].flags, 0);
    assert_eq!(humans.len(), 1);
}
#[test]
fn own_spectator_space_and_rejected_snapshots_never_mutate_the_visible_batch() {
    let mut humans = RemoteHumans::new(4).unwrap();
    let mut output = [empty(); 4];
    let own = player(1);
    let remote = player(2);
    let mut spectator = player(3);
    spectator.mode = 3;
    let mut other_space = player(4);
    other_space.space = SpaceId(1);
    assert_eq!(
        humans.write(
            Tick(1),
            1.,
            [&own, &remote, &spectator, &other_space].into_iter(),
            own.identity,
            SpaceId(0),
            [0; 3],
            &mut output
        ),
        Ok(1)
    );
    let old = output;
    let mut invalid = remote;
    invalid.position[0] = f64::NAN;
    assert_eq!(
        humans.write(
            Tick(2),
            2.,
            [&invalid].into_iter(),
            own.identity,
            SpaceId(0),
            [0; 3],
            &mut output
        ),
        Err(Error::Invalid)
    );
    assert_eq!(output, old);
    assert_eq!(humans.len(), 1);
    assert_eq!(
        humans.write(
            Tick(0),
            0.,
            [&remote].into_iter(),
            own.identity,
            SpaceId(0),
            [0; 3],
            &mut output
        ),
        Err(Error::Stale)
    );
    humans
        .write(
            Tick(2),
            2.,
            std::iter::empty(),
            own.identity,
            SpaceId(0),
            [0; 3],
            &mut output,
        )
        .unwrap();
    assert!(humans.is_empty());
}
#[test]
fn integer_origin_precedes_narrowing_and_teleports_never_interpolate_across_the_world() {
    let mut humans = RemoteHumans::new(4).unwrap();
    let mut output = [empty(); 4];
    let mut state = player(1);
    state.position = [1_000_000_000.125, 2., -1_000_000_000.25];
    humans
        .write(
            Tick(10),
            10.,
            [&state].into_iter(),
            PlayerId::default(),
            SpaceId(0),
            [1_000_000_000, 0, -1_000_000_000],
            &mut output,
        )
        .unwrap();
    assert_eq!(output[0].position, [0.125, 2., -0.25]);
    state.position[0] += 100.;
    humans
        .write(
            Tick(12),
            11.,
            [&state].into_iter(),
            PlayerId::default(),
            SpaceId(0),
            [1_000_000_000, 0, -1_000_000_000],
            &mut output,
        )
        .unwrap();
    assert_eq!(output[0].position[0], 100.125);
    assert_eq!(output[0].limb_amount, 0.);
}
#[test]
fn thirty_two_retained_public_bodies_run_ten_thousand_static_frames_without_allocations() {
    let mut humans = RemoteHumans::new(64).unwrap();
    let mut output = [empty(); 64];
    let players: Vec<_> = (1..=32).map(player).collect();
    humans
        .write(
            Tick(1),
            1.,
            players.iter(),
            PlayerId::default(),
            SpaceId(0),
            [0; 3],
            &mut output,
        )
        .unwrap();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        assert_eq!(
            humans.write(
                Tick(1),
                1.,
                players.iter(),
                PlayerId::default(),
                SpaceId(0),
                [0; 3],
                &mut output
            ),
            Ok(32)
        );
    }
    let work = scope.finish();
    assert_eq!(work.allocating_calls(), 0);
    assert_eq!(work.dealloc_calls, 0);
}
