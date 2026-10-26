use client::{
    audio_bridge::CausalSounds,
    network::{Connection, NetworkConfig},
};
use core_types::{SpaceId, Tick};
use net::{
    ActionId, PlayerId, PlayerState, Role, ServerMessage, Welcome,
    audio_cue::{Cue, Kind},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn connection(session: u64) -> Connection {
    let mut network = Connection::new(NetworkConfig::default()).unwrap();
    let identity = PlayerId([1; 16]);
    let sim = sim::Player::new([0.; 3]);
    let player = PlayerState {
        identity,
        position: sim.pos,
        velocity: sim.vel,
        yaw: sim.yaw,
        pitch: sim.pitch,
        health: sim.health,
        mode: sim.mode as u8,
        on_ground: sim.on_ground,
        flying: sim.flying,
        noclip: sim.noclip,
        ..Default::default()
    };
    network
        .receive(
            ServerMessage::Welcome(Welcome {
                session,
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
            5000,
        )
        .unwrap();
    network
        .receive(
            ServerMessage::Prediction {
                tick: Tick(101),
                player,
                continuation: sim::prediction::PredictionState::capture(&sim)
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            },
            5050,
        )
        .unwrap();
    network.advance_to(Tick(101)).unwrap();
    network
}
fn event(network: &mut Connection, actor: u64, sequence: u64, space: SpaceId) {
    let message = ServerMessage::AudioCue(Cue {
        kind: Kind::ChickenEgg,
        actor,
        sequence,
        tick: Tick(sequence),
        space,
        position: [1., 0., 0.],
    });
    network
        .receive(
            net::decode_server(&net::encode_server(&message).unwrap()).unwrap(),
            5100,
        )
        .unwrap();
}
#[test]
fn durable_causal_wire_reaches_real_egg_pcm_once_without_item_inference_or_idle_allocations() {
    let mut network = connection(1);
    let mut sounds = CausalSounds::default();
    assert_eq!(
        sounds.visit(&network, [0.; 3], |_| panic!("no causal event")),
        0
    );
    event(&mut network, 7, 102, SpaceId(0));
    event(&mut network, 7, 102, SpaceId(0));
    let prepared = audio::scene::PreparedSounds::prepare(None, None).unwrap();
    let (mut mixer, events) = prepared.into_parts();
    assert_eq!(
        sounds.visit(&network, [0.; 3], |cue| {
            assert_eq!(cue.event, "entity.chicken.egg");
            assert_eq!(cue.position, [1., 0., 0.]);
            let request = events
                .request(cue.event, cue.emitter, Some(cue.position), cue.seed)
                .unwrap();
            assert_eq!(request.category, audio::Category::Entities);
            mixer.play(request).unwrap();
        }),
        1
    );
    let mut pcm = [0.; 2048];
    let mut audible = false;
    for _ in 0..8 {
        mixer.mix_stereo(&mut pcm);
        audible |= pcm.iter().any(|sample| sample.abs() > 1e-6);
    }
    assert!(audible, "decoded original egg event produced no actual PCM");
    event(&mut network, 8, 102, SpaceId(1));
    assert_eq!(
        sounds.visit(&network, [0.; 3], |_| panic!("foreign space")),
        0
    );
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10000 {
        assert_eq!(
            sounds.visit(&network, [0.; 3], |_| panic!("replayed cue")),
            0
        );
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    let mut resumed = connection(2);
    event(&mut resumed, 7, 102, SpaceId(0));
    assert_eq!(sounds.visit(&resumed, [0.; 3], |_| {}), 1);
    assert_eq!(sounds.emitted, 2);
}

#[test]
fn owned_cue_kinds_cross_the_actual_wire_and_select_decoded_pcm_exactly_once() {
    let mut network = connection(3);
    let mut sounds = CausalSounds::default();
    let prepared = audio::scene::PreparedSounds::prepare(None, None).unwrap();
    let (mut mixer, events) = prepared.into_parts();
    for (ordinal, (kind, expected)) in [
        (Kind::ChestOpen, "chest.open"),
        (Kind::ChestClose, "chest.close"),
        (Kind::ItemPickup, "item.pickup"),
        (Kind::AttackWeak, "player.attack_weak"),
        (Kind::AttackStrong, "player.attack_strong"),
        (Kind::AttackCrit, "player.attack_crit"),
        (Kind::AttackKnockback, "player.attack_knockback"),
        (Kind::BucketFill, "bucket.fill"),
        (Kind::BucketEmpty, "bucket.empty"),
        (Kind::Eat, "player.eat"),
        (Kind::EatDone, "player.eat_done"),
    ]
    .into_iter()
    .enumerate()
    {
        let cue = Cue {
            kind,
            actor: 7,
            sequence: if kind == Kind::ItemPickup {
                102
            } else {
                ordinal as u64 + 1
            },
            tick: Tick(102),
            space: SpaceId(0),
            position: [1., 0., 0.],
        };
        let message = ServerMessage::AudioCue(cue);
        for _ in 0..2 {
            network
                .receive(
                    net::decode_server(&net::encode_server(&message).unwrap()).unwrap(),
                    5100,
                )
                .unwrap();
        }
        let mut voice = None;
        assert_eq!(
            sounds.visit(&network, [0.; 3], |cue| {
                assert_eq!(cue.event, expected);
                voice = Some(
                    mixer
                        .play(
                            events
                                .request(cue.event, cue.emitter, Some(cue.position), cue.seed)
                                .unwrap(),
                        )
                        .unwrap(),
                );
            }),
            1
        );
        let mut pcm = [0.; 2048];
        let mut audible = false;
        for _ in 0..8 {
            mixer.mix_stereo(&mut pcm);
            audible |= pcm.iter().any(|sample| sample.abs() > 1e-6);
        }
        assert!(audible, "{expected} must select actual decoded PCM");
        mixer.stop(voice.unwrap());
        assert_eq!(
            sounds.visit(&network, [0.; 3], |_| panic!("duplicate {expected}")),
            0
        );
    }
    assert_eq!(network.audio_cues().counts().duplicate, 11);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10000 {
        assert_eq!(
            sounds.visit(&network, [0.; 3], |_| panic!("replayed cue")),
            0
        );
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
}
