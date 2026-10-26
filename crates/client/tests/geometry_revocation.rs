use client::network::{Connection, NetworkConfig};
use core_types::{PageId, SpaceId, Version};
use net::{PublicGeometry, ServerMessage, height::HeightKey};

fn key() -> HeightKey {
    HeightKey {
        space: 0,
        level: 0,
        x: 100,
        z: 70,
    }
}
fn geometry(version: u64, y: i32) -> PublicGeometry {
    PublicGeometry {
        page: PageId {
            space: SpaceId(0),
            x: 100,
            y,
            z: 70,
        },
        source_version: Version(version),
        view_version: version,
        known: [255; 512],
        occupied: [0; 512],
        palette: Vec::new(),
        index_bits: 0,
        indices: Vec::new(),
    }
}
fn wire(client: &mut Connection, message: ServerMessage, now: u64) {
    let encoded = net::encode_server(&message).unwrap();
    client
        .receive(net::decode_server(&encoded).unwrap(), now)
        .unwrap();
}
fn client() -> Connection {
    let mut client = Connection::new(NetworkConfig::default()).unwrap();
    client.request_height(key(), 0).unwrap();
    client.take_outgoing(256);
    client
}

#[test]
fn queued_geometry_cannot_undo_a_later_encoded_revocation() {
    let mut client = client();
    let first = geometry(1, 0);
    let unrelated = geometry(1, 1);
    wire(
        &mut client,
        ServerMessage::Geometry(Box::new(first.clone())),
        1,
    );
    wire(
        &mut client,
        ServerMessage::Geometry(Box::new(unrelated.clone())),
        2,
    );
    wire(
        &mut client,
        ServerMessage::GeometryUnavailable(first.page),
        3,
    );
    assert!(client.geometry(first.page).is_none());
    // This is the actual Remote courier order. An older queued image must not
    // be installed immediately after the page's removal was consumed.
    assert_eq!(client.take_geometry_removal(), Some(first.page));
    let updates = client.take_geometry_updates(32);
    assert_eq!(updates, vec![unrelated]);
}

#[test]
fn a_fresh_encoded_image_after_revocation_is_still_delivered() {
    let mut client = client();
    let first = geometry(1, 0);
    let newer = geometry(2, 0);
    wire(
        &mut client,
        ServerMessage::Geometry(Box::new(first.clone())),
        1,
    );
    wire(
        &mut client,
        ServerMessage::GeometryUnavailable(first.page),
        2,
    );
    wire(
        &mut client,
        ServerMessage::Geometry(Box::new(newer.clone())),
        3,
    );
    assert_eq!(client.take_geometry_removal(), Some(first.page));
    assert_eq!(client.take_geometry_updates(32), vec![newer.clone()]);
    assert_eq!(client.geometry(newer.page), Some(&newer));
}
