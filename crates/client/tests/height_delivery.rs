use client::network::{Connection, NetworkConfig, NetworkError};
use core_types::{PageId, SpaceId, Version};
use net::{ClientMessage, PublicGeometry, ServerMessage, height::HeightKey};
fn key() -> HeightKey {
    HeightKey {
        space: 0,
        level: 0,
        x: 100,
        z: 70,
    }
}
fn geometry(y: i32) -> PublicGeometry {
    PublicGeometry {
        page: PageId {
            space: SpaceId(0),
            x: 100,
            y,
            z: 70,
        },
        source_version: Version(3),
        view_version: y as u64 + 1,
        known: [255; 512],
        occupied: [0; 512],
        palette: Vec::new(),
        index_bits: 0,
        indices: Vec::new(),
    }
}
#[test]
fn height_column_geometry_arriving_before_manifest_is_cached_without_raw_near_pages() {
    let mut client = Connection::new(NetworkConfig::default()).unwrap();
    client.request_height(key(), 0).unwrap();
    let mut outside = geometry(0);
    outside.page.x += 1;
    client
        .receive(ServerMessage::Geometry(Box::new(outside.clone())), 1)
        .unwrap();
    assert!(client.geometry(outside.page).is_none());
    for y in 0..8 {
        let g = geometry(y);
        client
            .receive(ServerMessage::Geometry(Box::new(g.clone())), 2)
            .unwrap();
        assert_eq!(client.geometry(g.page), Some(&g));
    }
    assert_eq!(client.take_geometry_updates(32).len(), 8);
    assert_eq!(
        client.replica().len(),
        0,
        "height geometry never creates raw near replicas"
    );
    // An explicit protected request keeps its ownership after the horizon leaf
    // leaves AOI; the other seven implicit subscriptions are released together.
    let first = geometry(0).page;
    client.request_geometry(first, 3).unwrap();
    client.release_height(key()).unwrap();
    assert!(client.geometry(first).is_some());
    for y in 1..8 {
        assert!(client.geometry(geometry(y).page).is_none());
    }
    let outgoing = client.take_outgoing(256);
    assert_eq!(
        outgoing
            .iter()
            .filter(|m| matches!(m, ClientMessage::ForgetGeometry(_)))
            .count(),
        7
    );
}
#[test]
fn conflicting_identical_geometry_version_fails_atomically_and_duplicates_are_quiet() {
    let mut client = Connection::new(NetworkConfig::default()).unwrap();
    client.request_height(key(), 0).unwrap();
    let g = geometry(0);
    client
        .receive(ServerMessage::Geometry(Box::new(g.clone())), 1)
        .unwrap();
    client.take_geometry_updates(32);
    client
        .receive(ServerMessage::Geometry(Box::new(g.clone())), 2)
        .unwrap();
    assert!(client.take_geometry_updates(32).is_empty());
    let mut corrupt = g.clone();
    corrupt.known[0] = 0;
    assert!(matches!(
        client.receive(ServerMessage::Geometry(Box::new(corrupt)), 3),
        Err(NetworkError::Protocol)
    ));
    assert_eq!(client.geometry(g.page), Some(&g));
}
