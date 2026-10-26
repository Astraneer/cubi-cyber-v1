//! A representation upgrade is sticky across retries and crossed ordinary images.
use client::network::{Connection, NetworkConfig};
use net::{
    ClientMessage, ServerMessage,
    height::{HeightKey, HeightTile, Replacement},
};
fn tile() -> HeightTile {
    HeightTile {
        key: HeightKey {
            space: 0,
            level: 0,
            x: 0,
            z: 0,
        },
        source: 7,
        error: None,
        heights: [0; 289],
        known: [0; 37],
        atlas: [0; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
        replacement: Replacement::None,
    }
}
#[test]
fn exact_promotion_supersedes_unsent_normal_request_and_never_downgrades() {
    let mut connection = Connection::new(NetworkConfig::default()).unwrap();
    let t = tile();
    connection.request_height(t.key, 0).unwrap();
    connection.request_height_exact(t.key, 1).unwrap();
    let outgoing = connection.take_outgoing(256);
    assert_eq!(outgoing.len(), 1);
    assert!(matches!(
        outgoing[0],
        ClientMessage::RequestExactHeight { known: None, .. }
    ));
    connection
        .receive(ServerMessage::Height(Box::new(t.clone())), 2)
        .unwrap();
    assert!(connection.take_height_update().is_none());
    connection.request_height(t.key, 1001).unwrap();
    assert!(matches!(
        connection.take_outgoing(1)[0],
        ClientMessage::RequestExactHeight { known: None, .. }
    ));
    let mut promoted = t.clone();
    promoted.protected = [255; 32];
    promoted.replacement = Replacement::Column {
        min_y: 0,
        max_y: 7,
        geometry: Vec::new(),
    };
    connection
        .receive(ServerMessage::Height(Box::new(promoted.clone())), 1002)
        .unwrap();
    connection
        .receive(ServerMessage::Height(Box::new(t)), 1003)
        .unwrap();
    assert_eq!(*connection.take_height_update().unwrap(), promoted);
    connection.release_height(promoted.key).unwrap();
    assert!(!connection.height_is_exact(promoted.key));
    connection.take_outgoing(256);
    connection.request_height(promoted.key, 2000).unwrap();
    assert!(matches!(
        connection.take_outgoing(1)[0],
        ClientMessage::RequestHeight { known: None, .. }
    ));
}
