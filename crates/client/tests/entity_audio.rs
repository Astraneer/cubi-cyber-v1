use client::{
    audio_bridge::EntitySounds,
    network::{Connection, NetworkConfig},
};
use core_types::{SpaceId, Tick};
use net::{
    ActionId, PlayerId, PlayerState, Role, ServerMessage, Welcome,
    entities::{EntityFrame, EntityPose},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn connection() -> Connection {
    let mut connection = Connection::new(NetworkConfig::default()).unwrap();
    connection
        .receive(
            ServerMessage::Welcome(Welcome {
                session: 1,
                identity: PlayerId([1; 16]),
                new_credential: None,
                role: Role::Player,
                tick: Tick(0),
                next_action: ActionId(1),
                public_seed: 1,
                generator_version: terrain_gen::GENERATOR_VERSION,
                rules_version: net::RULES_VERSION,
                player: PlayerState {
                    identity: PlayerId([1; 16]),
                    health: 20.,
                    ..Default::default()
                },
            }),
            0,
        )
        .unwrap();
    connection
}
fn pose(id: u64, kind: u8) -> EntityPose {
    EntityPose {
        id,
        kind,
        space: SpaceId(0),
        position: [0.; 3],
        body_yaw: 0.,
        head_yaw: 0.,
        pitch: 0.,
        limb_phase: 0.,
        limb_amount: 0.,
        death_roll: 0.,
        scale: 1.,
        extra: 0.,
        flags: 64,
        color: 0,
        count: 0,
        damage: 0,
    }
}
fn frame(connection: &mut Connection, revision: u64, poses: Vec<EntityPose>) {
    let message = ServerMessage::Entities(Box::new(EntityFrame {
        revision,
        base: None,
        tick: Tick(revision),
        upserts: poses,
        removes: Vec::new(),
    }));
    // The same strict public codec used by both actual transports supplies cues.
    connection
        .receive(
            net::decode_server(&net::encode_server(&message).unwrap()).unwrap(),
            revision * 50,
        )
        .unwrap();
    let _ = connection.take_outgoing(256);
}
#[test]
fn public_transitions_are_positional_once_and_initial_images_other_spaces_and_drops_are_silent() {
    let mut network = connection();
    let mut sounds = EntitySounds::default();
    let mut pig = pose(1, 0);
    let mut sheep = pose(3, 2);
    let mut other = pose(4, 4);
    other.space = SpaceId(1);
    let mut drop = pose(5, 7);
    drop.flags = 32;
    drop.color = 100;
    drop.count = 1;
    frame(&mut network, 1, vec![pig, sheep, other, drop]);
    assert_eq!(
        sounds.visit(&network, [0.; 3], |_| panic!("first image must be silent")),
        0
    );
    pig.flags |= 16;
    pig.limb_phase = 5.;
    pig.limb_amount = 1.;
    sheep.flags |= 2;
    other.flags |= 16;
    drop.flags |= 16;
    frame(&mut network, 2, vec![pig, sheep, other, drop]);
    let mut cues = Vec::new();
    assert_eq!(sounds.visit(&network, [0.; 3], |cue| cues.push(cue)), 3);
    assert_eq!(
        cues.iter().map(|c| c.event).collect::<Vec<_>>(),
        vec!["entity.pig.hurt", "entity.pig.step", "entity.sheep.shear"]
    );
    assert!(
        cues.iter()
            .all(|c| c.position == [0.; 3] && c.emitter.0 & 3 == 2)
    );
    assert_eq!(
        sounds.visit(&network, [0.; 3], |_| panic!("duplicate revision")),
        0
    );
    pig.flags |= 4;
    pig.death_roll = 20.;
    frame(&mut network, 3, vec![pig, sheep, other]);
    cues.clear();
    sounds.visit(&network, [0.; 3], |cue| cues.push(cue));
    assert_eq!(
        cues.iter().map(|c| c.event).collect::<Vec<_>>(),
        vec!["entity.pig.death"]
    );
    // Re-entry must establish a new silent baseline instead of replaying death.
    frame(&mut network, 4, vec![sheep]);
    sounds.visit(&network, [0.; 3], |_| {});
    frame(&mut network, 5, vec![pig, sheep]);
    assert_eq!(sounds.visit(&network, [0.; 3], |_| panic!("re-entry")), 0);
}
#[test]
fn baby_limb_frequency_matches_the_public_model_and_paused_motion_is_silent() {
    let mut network = connection();
    let mut sounds = EntitySounds::default();
    let mut baby = pose(1, 0);
    baby.flags |= 1;
    baby.limb_amount = 1.;
    let mut adult = pose(2, 0);
    adult.limb_amount = 1.;
    frame(&mut network, 1, vec![baby, adult]);
    sounds.visit(&network, [0.; 3], |_| {});
    baby.limb_phase = 2.;
    adult.limb_phase = 2.;
    frame(&mut network, 2, vec![baby, adult]);
    let mut cues = Vec::new();
    sounds.visit(&network, [0.; 3], |cue| cues.push(cue));
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].event, "entity.pig.step");
    assert_eq!(cues[0].emitter.0, 6);
    frame(&mut network, 3, vec![baby, adult]);
    assert_eq!(
        sounds.visit(&network, [0.; 3], |_| panic!("unchanged phase")),
        0
    );
    assert_eq!(
        sounds.visit(&network, [f64::NAN, 0., 0.], |_| panic!("invalid listener")),
        0
    );
}
#[test]
fn simultaneous_work_is_bounded_and_unchanged_revisions_allocate_nothing() {
    let mut network = connection();
    let mut sounds = EntitySounds::default();
    let poses = (1..=32).map(|id| pose(id, 0)).collect::<Vec<_>>();
    frame(&mut network, 1, poses.clone());
    sounds.visit(&network, [0.; 3], |_| {});
    frame(
        &mut network,
        2,
        poses
            .into_iter()
            .map(|mut p| {
                p.flags |= 16;
                p
            })
            .collect(),
    );
    assert_eq!(sounds.visit(&network, [0.; 3], |_| {}), 16);
    assert_eq!(sounds.dropped, 16);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        assert_eq!(sounds.visit(&network, [0.; 3], |_| panic!("unchanged")), 0);
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    sounds.clear();
    assert_eq!(
        sounds.visit(&network, [0.; 3], |_| panic!("new session baseline")),
        0
    );
}
