use client::{
    audio_bridge::ConfirmedBlockSounds,
    network::{Connection, NetworkConfig},
};
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use net::{
    Action, ActionId, ActionReply, ActionStatus, PlayerId, PlayerState, RejectReason, Role,
    ServerMessage, Welcome,
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn connection(session: u64) -> Connection {
    let mut network = Connection::new(NetworkConfig::default()).unwrap();
    network
        .receive(
            ServerMessage::Welcome(Welcome {
                session,
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
                    health: 20.0,
                    ..Default::default()
                },
            }),
            0,
        )
        .unwrap();
    network
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
fn accepted_effect_is_once_rejections_are_silent_and_other_spaces_are_excluded() {
    let mut network = connection(1);
    let mut sounds = ConfirmedBlockSounds::default();
    let position = BlockPos {
        space: SpaceId(0),
        x: 12,
        y: 80,
        z: 4,
    };
    let id = network
        .action(
            Action::Place {
                position,
                expected: Cell::AIR,
                cell: Cell::from_parts(rules::ids::CHEST, 0),
            },
            false,
            0,
        )
        .unwrap();
    network
        .action(
            Action::Place {
                position: BlockPos {
                    space: SpaceId(1),
                    ..position
                },
                expected: Cell::AIR,
                cell: Cell::from_parts(rules::ids::STONE, 0),
            },
            false,
            0,
        )
        .unwrap();
    assert_eq!(network.pending_block_sounds().count(), 1);
    sounds.observe(&network);
    assert!(sounds.reply(reply(id, ActionStatus::Pending)).is_none());
    network
        .receive(ServerMessage::Action(reply(id, ActionStatus::Accepted)), 1)
        .unwrap();
    sounds.observe(&network);
    let cue = sounds.reply(network.take_reply().unwrap()).unwrap();
    assert_eq!(cue.event, "block.wood.step");
    assert_eq!(cue.gain, 1.0);
    assert_eq!(cue.position, [12.5, 80.5, 4.5]);
    assert!(sounds.reply(reply(id, ActionStatus::Accepted)).is_none());
    assert_eq!(sounds.played(), 1);
    let id = network
        .action(
            Action::Mine {
                position,
                expected: Cell::from_parts(rules::ids::GLASS, 0),
                tool: 0,
            },
            false,
            2,
        )
        .unwrap();
    sounds.observe(&network);
    network
        .receive(
            ServerMessage::Action(reply(id, ActionStatus::Rejected(RejectReason::OutOfReach))),
            3,
        )
        .unwrap();
    assert!(sounds.reply(network.take_reply().unwrap()).is_none());
    assert_eq!(sounds.played(), 1);
    sounds.clear();
    assert!(sounds.reply(reply(id, ActionStatus::Accepted)).is_none());
}
#[test]
fn observing_steady_pending_actions_allocates_nothing_and_session_change_clears_old_ids() {
    let mut network = connection(1);
    let mut sounds = ConfirmedBlockSounds::default();
    let id = network
        .action(
            Action::Mine {
                position: BlockPos {
                    space: SpaceId(0),
                    x: 1,
                    y: 2,
                    z: 3,
                },
                expected: Cell::from_parts(rules::ids::STONE, 0),
                tool: 0,
            },
            false,
            0,
        )
        .unwrap();
    sounds.observe(&network);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        sounds.observe(&network);
    }
    let count = scope.finish();
    assert_eq!(count.allocating_calls(), 0);
    assert_eq!(count.dealloc_calls, 0);
    assert_eq!(sounds.overflow(), 0);
    let next = connection(2);
    sounds.observe(&next);
    assert!(sounds.reply(reply(id, ActionStatus::Accepted)).is_none());
    assert_eq!(sounds.played(), 0);
}
