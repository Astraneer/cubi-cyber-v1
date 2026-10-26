use client::network::{Connection, NetworkConfig, NetworkError};
use core_types::{BlockPos, SpaceId, Tick};
use net::{ActionId, PlayerState, ServerMessage, Welcome, spawn::Landmark};

fn welcome(session: u64) -> ServerMessage {
    ServerMessage::Welcome(Welcome {
        session,
        identity: Default::default(),
        new_credential: None,
        role: net::Role::Player,
        tick: Tick(1),
        next_action: ActionId(1),
        public_seed: 1,
        generator_version: terrain_gen::GENERATOR_VERSION,
        rules_version: net::RULES_VERSION,
        // A saved player can log in far from the public square.
        player: PlayerState {
            position: [2000.5, 90., -3000.5],
            health: 20.,
            ..Default::default()
        },
    })
}
fn receive(c: &mut Connection, message: ServerMessage) -> Result<(), NetworkError> {
    let bytes = net::encode_server(&message).unwrap();
    c.receive(net::decode_server(&bytes).unwrap(), 100)
}

#[test]
fn square_requires_matching_session_and_cannot_be_relocated_by_duplicates() {
    let mut c = Connection::new(NetworkConfig::default()).unwrap();
    let value = Landmark {
        session: 3,
        center: BlockPos {
            space: SpaceId(0),
            x: 32,
            y: 64,
            z: -48,
        },
    };
    assert!(matches!(
        receive(&mut c, ServerMessage::SpawnLandmark(value)),
        Err(NetworkError::NotConnected)
    ));
    assert_eq!(c.spawn_landmark(), None);
    receive(&mut c, welcome(3)).unwrap();
    receive(&mut c, ServerMessage::SpawnLandmark(value)).unwrap();
    receive(&mut c, ServerMessage::SpawnLandmark(value)).unwrap();
    receive(&mut c, welcome(3)).unwrap();
    assert_eq!(c.spawn_landmark(), Some(value));
    for invalid in [
        Landmark {
            session: 2,
            ..value
        },
        Landmark {
            center: BlockPos {
                space: SpaceId(0),
                x: 33,
                y: 64,
                z: -48,
            },
            ..value
        },
    ] {
        assert!(matches!(
            receive(&mut c, ServerMessage::SpawnLandmark(invalid)),
            Err(NetworkError::Protocol)
        ));
        assert_eq!(c.spawn_landmark(), Some(value));
    }
    let mut replacement = Connection::new(NetworkConfig::default()).unwrap();
    receive(&mut replacement, welcome(4)).unwrap();
    assert_eq!(replacement.spawn_landmark(), None);
    assert!(receive(&mut replacement, ServerMessage::SpawnLandmark(value)).is_err());
}
