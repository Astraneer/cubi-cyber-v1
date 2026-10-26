//! Encoded durable block events use one owner/observer path, including a reply
//! which beats presentation's first observation of the pending action.
use client::{
    audio_bridge::{CausalSounds, ConfirmedBlockSounds},
    network::{Connection, NetworkConfig},
};
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use net::{
    Action, ActionId, ActionReply, ActionStatus, PlayerId, PlayerState, Role, ServerMessage,
    Welcome,
    audio_cue::{BlockActor, BlockMaterial, Cue, Kind},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn connection(session: u64) -> Connection {
    let mut network = Connection::new(NetworkConfig::default()).unwrap();
    let player = sim::Player::new([0.; 3]);
    let state = PlayerState {
        identity: PlayerId([1; 16]),
        position: player.pos,
        velocity: player.vel,
        yaw: player.yaw,
        pitch: player.pitch,
        health: player.health,
        mode: player.mode as u8,
        on_ground: player.on_ground,
        flying: player.flying,
        noclip: player.noclip,
        ..Default::default()
    };
    for message in [
        ServerMessage::Welcome(Welcome {
            session,
            identity: state.identity,
            new_credential: None,
            role: Role::Player,
            tick: Tick(100),
            next_action: ActionId(1),
            public_seed: 1,
            generator_version: terrain_gen::GENERATOR_VERSION,
            rules_version: net::RULES_VERSION,
            player: state,
        }),
        ServerMessage::Prediction {
            tick: Tick(101),
            player: state,
            continuation: sim::prediction::PredictionState::capture(&player)
                .unwrap()
                .as_bytes()
                .to_vec(),
        },
    ] {
        receive(&mut network, message);
    }
    network.advance_to(Tick(101)).unwrap();
    network
}
fn receive(network: &mut Connection, message: ServerMessage) {
    network
        .receive(
            net::decode_server(&net::encode_server(&message).unwrap()).unwrap(),
            5050,
        )
        .unwrap();
}
fn cue(player: PlayerId, action: ActionId, breaking: bool, tick: u64) -> Cue {
    let actor = BlockActor {
        player,
        material: if breaking {
            BlockMaterial::Glass
        } else {
            BlockMaterial::Wood
        },
    };
    Cue {
        kind: if breaking {
            Kind::BlockBreak(actor)
        } else {
            Kind::BlockPlace(actor)
        },
        actor: net::player_entity_id(player),
        sequence: action.0,
        tick: Tick(tick),
        space: SpaceId(0),
        position: [1.5, 2.5, 3.5],
    }
}
fn reply(id: ActionId, status: ActionStatus) -> ActionReply {
    ActionReply {
        id,
        status,
        transaction: None,
        source_version: Version(0),
        position: None,
        cell: None,
    }
}
#[test]
fn own_fast_replies_and_remote_events_reach_original_pcm_once_without_prediction_double() {
    let mut network = connection(1);
    let mut local = ConfirmedBlockSounds::default();
    let mut causal = CausalSounds::default();
    let position = BlockPos {
        space: SpaceId(0),
        x: 1,
        y: 2,
        z: 3,
    };
    let prepared = audio::scene::PreparedSounds::prepare(None, None).unwrap();
    let (mut mixer, events) = prepared.into_parts();
    let mut played = Vec::new();
    for (breaking, tick) in [(true, 102), (false, 103)] {
        let action = if breaking {
            Action::Mine {
                position,
                expected: Cell::from_parts(rules::ids::GLASS, 0),
                tool: 0,
            }
        } else {
            Action::Place {
                position,
                expected: Cell::AIR,
                cell: Cell::from_parts(rules::ids::CHEST, 0),
            }
        };
        let id = network.action(action, false, 5050).unwrap();
        // SharedSocket may already have removed this pending action before a
        // slow render owner first calls observe(). No local sound is invented.
        receive(
            &mut network,
            ServerMessage::Action(reply(id, ActionStatus::Accepted)),
        );
        local.observe(&network);
        assert_eq!(network.pending_block_sounds().count(), 0);
        assert!(local.reply(network.take_reply().unwrap()).is_none());
        let authoritative = cue(PlayerId([1; 16]), id, breaking, tick);
        for _ in 0..2 {
            receive(&mut network, ServerMessage::AudioCue(authoritative));
        }
        assert_eq!(
            causal.visit(&network, [0.; 3], |cue| {
                played.push(cue.event);
                mixer
                    .play(
                        events
                            .request(cue.event, cue.emitter, Some(cue.position), cue.seed)
                            .unwrap(),
                    )
                    .unwrap();
            }),
            1
        );
        assert_eq!(
            causal.visit(&network, [0.; 3], |_| panic!("own sound repeated")),
            0
        );
    }
    assert_eq!(played, ["block.glass.break", "block.wood.step"]);
    // Equal action IDs from another exact public UUID are independent.
    receive(
        &mut network,
        ServerMessage::AudioCue(cue(PlayerId([2; 16]), ActionId(1), true, 103)),
    );
    assert_eq!(
        causal.visit(&network, [0.; 3], |cue| {
            played.push(cue.event);
            mixer
                .play(
                    events
                        .request(cue.event, cue.emitter, Some(cue.position), cue.seed)
                        .unwrap(),
                )
                .unwrap();
        }),
        1
    );
    let mut samples = [0.; 2048];
    let mut nonzero = false;
    for _ in 0..8 {
        mixer.mix_stereo(&mut samples);
        nonzero |= samples.iter().any(|v| v.abs() > 1e-6);
    }
    assert!(nonzero, "decoded original block events produced no PCM");
    assert_eq!(
        local.played(),
        0,
        "legacy pending path must not double own Mine/Place"
    );
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        assert_eq!(causal.visit(&network, [0.; 3], |_| panic!("idle")), 0);
        local.observe(&network);
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    let next = connection(2);
    assert_eq!(
        causal.visit(&next, [0.; 3], |_| panic!("reconnect replay")),
        0
    );
}
#[test]
fn rejected_action_unknown_event_absence_foreign_space_and_stale_cues_are_silent() {
    let mut network = connection(1);
    let mut causal = CausalSounds::default();
    let id = network
        .action(
            Action::Mine {
                position: BlockPos {
                    space: SpaceId(0),
                    x: 1,
                    y: 2,
                    z: 3,
                },
                expected: Cell::from_parts(rules::ids::GLASS, 0),
                tool: 0,
            },
            false,
            5050,
        )
        .unwrap();
    receive(
        &mut network,
        ServerMessage::Action(reply(
            id,
            ActionStatus::Rejected(net::RejectReason::Occluded),
        )),
    );
    assert_eq!(
        causal.visit(&network, [0.; 3], |_| panic!("rejection inferred a sound")),
        0
    );
    receive(
        &mut network,
        ServerMessage::AudioCue(Cue {
            space: SpaceId(2),
            ..cue(PlayerId([2; 16]), id, true, 102)
        }),
    );
    assert_eq!(
        causal.visit(&network, [0.; 3], |_| panic!("foreign space")),
        0
    );
    receive(
        &mut network,
        ServerMessage::AudioCue(cue(PlayerId([2; 16]), id, true, 1)),
    );
    assert_eq!(causal.visit(&network, [0.; 3], |_| panic!("stale")), 0);
}

#[test]
fn all_wire_sound_families_preserve_the_existing_break_and_place_table() {
    use BlockMaterial::*;
    let rows = [
        (Stone, "block.stone.dig", "block.stone.step"),
        (Wood, "block.wood.dig", "block.wood.step"),
        (Gravel, "block.gravel.dig", "block.gravel.step"),
        (Grass, "block.grass.dig", "block.grass.step"),
        (Sand, "block.sand.dig", "block.sand.step"),
        (Snow, "block.snow.dig", "block.snow.step"),
        (Wool, "block.cloth.dig", "block.cloth.step"),
        (Glass, "block.glass.break", "block.stone.step"),
        (Metal, "block.stone.dig", "block.stone.step"),
        (Ladder, "block.wood.dig", "block.ladder.step"),
    ];
    let mut network = connection(1);
    let mut sounds = CausalSounds::default();
    let mut expected = Vec::new();
    let mut sequence = 0;
    for (material, breaking, placing) in rows {
        for (is_break, event) in [(true, breaking), (false, placing)] {
            sequence += 1;
            let mut value = cue(PlayerId([2; 16]), ActionId(sequence), is_break, 102);
            let block = BlockActor {
                player: PlayerId([2; 16]),
                material,
            };
            value.kind = if is_break {
                Kind::BlockBreak(block)
            } else {
                Kind::BlockPlace(block)
            };
            receive(&mut network, ServerMessage::AudioCue(value));
            expected.push(event);
        }
    }
    let mut actual = Vec::new();
    while sounds.visit(&network, [0.; 3], |cue| {
        assert_eq!(cue.gain, 1.);
        actual.push(cue.event);
    }) > 0
    {}
    assert_eq!(actual, expected);
}
