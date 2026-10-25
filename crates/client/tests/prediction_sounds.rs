use client::{
    audio_bridge::PredictionSounds,
    network::{Connection, NetworkConfig},
};
use core_types::{SpaceId, Tick};
use net::{ActionId, PlayerId, PlayerState, ServerMessage, Welcome};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn public(player: &sim::Player) -> PlayerState {
    PlayerState {
        identity: PlayerId([1; 16]),
        space: SpaceId(0),
        input_sequence: 0,
        position: player.pos,
        velocity: player.vel,
        yaw: player.yaw,
        pitch: player.pitch,
        health: player.health,
        mode: player.mode as u8,
        on_ground: player.on_ground,
        flying: player.flying,
        noclip: player.noclip,
    }
}
fn prediction(network: &mut Connection, tick: u64, health: f32) {
    let mut player = sim::Player::new([tick as f64 * 0.3, 1.0, 0.5]);
    player.prev = [(tick - 1) as f64 * 0.3, 1.0, 0.5];
    player.health = health;
    let continuation = sim::prediction::PredictionState::capture(&player)
        .unwrap()
        .as_bytes()
        .to_vec();
    network
        .receive(
            ServerMessage::Prediction {
                tick: Tick(tick),
                player: public(&player),
                continuation,
            },
            tick * 50,
        )
        .unwrap();
    assert!(network.advance_to(Tick(tick)).unwrap());
}
#[test]
fn catchup_visits_every_actual_tick_once_and_health_is_not_replayed() {
    let mut network = Connection::new(NetworkConfig::default()).unwrap();
    let player = sim::Player::new([0.0, 1.0, 0.5]);
    network
        .receive(
            ServerMessage::Welcome(Welcome {
                session: 1,
                identity: PlayerId([1; 16]),
                new_credential: None,
                role: net::Role::Player,
                tick: Tick(0),
                next_action: ActionId(1),
                public_seed: 1,
                generator_version: terrain_gen::GENERATOR_VERSION,
                rules_version: net::RULES_VERSION,
                player: public(&player),
            }),
            0,
        )
        .unwrap();
    let mut sounds = PredictionSounds::default();
    assert!(sounds.bind(&network).is_none());
    prediction(&mut network, 1, 20.0);
    let query = |_| world::CellRead::Known(core_types::Cell::from_parts(rules::ids::STONE, 0));
    assert_eq!(sounds.visit(&network, &query, |_, _, _, _| {}).unwrap(), 1);
    assert!(sounds.health_event(&network).is_none());
    assert!(network.advance_to(Tick(5)).unwrap());
    let scope = telemetry::AllocationScope::begin().unwrap();
    let mut expected = 2;
    assert_eq!(
        sounds
            .visit(&network, &query, |tick, body, _, _| {
                assert_eq!(tick.0, expected);
                expected += 1;
                assert_eq!(body.moved, [0.0; 3]);
            })
            .unwrap(),
        4
    );
    assert_eq!(
        sounds
            .visit(&network, &query, |_, _, _, _| panic!("replayed sound"))
            .unwrap(),
        0
    );
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    prediction(&mut network, 6, 10.0);
    assert_eq!(sounds.health_event(&network).unwrap().0, "player.hurt");
    assert!(sounds.health_event(&network).is_none());
    let offline = Connection::new(NetworkConfig::default()).unwrap();
    assert!(sounds.bind(&offline).is_some());
    assert!(sounds.emitter().is_none());
}
