//! Rendering clock is derived from public authority rather than input prediction.
use client::network::{Connection, NetworkConfig};
use core_types::Tick;
use net::{ActionId, PlayerId, PlayerState, Role, ServerMessage, Welcome};
#[test]
fn visual_clock_uses_confirmed_state_and_clamps_extrapolation_without_changing_prediction_time() {
    let mut c = Connection::new(NetworkConfig::default()).unwrap();
    assert_eq!(c.visual_server_tick_time(u64::MAX), 0.);
    let identity = PlayerId([1; 16]);
    let player = PlayerState {
        identity,
        health: 20.,
        position: [0.5, 1., 0.5],
        ..PlayerState::default()
    };
    c.receive(
        ServerMessage::Welcome(Welcome {
            session: 1,
            identity,
            new_credential: None,
            role: Role::Player,
            tick: Tick(100),
            next_action: ActionId(1),
            public_seed: 1,
            generator_version: terrain_gen::GENERATOR_VERSION,
            rules_version: net::RULES_VERSION,
            player,
        }),
        1000,
    )
    .unwrap();
    assert_eq!(c.visual_server_tick_time(1050), 101.);
    c.receive(
        ServerMessage::State {
            tick: Tick(105),
            player,
        },
        1100,
    )
    .unwrap();
    assert_eq!(c.visual_server_tick_time(1125), 105.5);
    assert_eq!(c.acknowledged_tick(), Tick(100));
    c.receive(
        ServerMessage::State {
            tick: Tick(104),
            player,
        },
        1200,
    )
    .unwrap();
    assert_eq!(c.visual_server_tick_time(1200), 107.);
    assert_eq!(c.visual_server_tick_time(u64::MAX), 107.);
    assert_eq!(c.acknowledged_tick(), Tick(100));
    let large_tick = Tick(u64::MAX - 200);
    let state = ServerMessage::State {
        tick: large_tick,
        player,
    };
    c.receive(
        net::decode_server(&net::encode_server(&state).unwrap()).unwrap(),
        2000,
    )
    .unwrap();
    assert_eq!(c.visual_clock_sample(2025), (large_tick, 0.5));
    assert_eq!(c.acknowledged_tick(), Tick(100));
}
