//! Slow loading/render owners retain one composable update per subscribed page.
use client::network::{Connection, NetworkConfig};
use core_types::{Cell, PageId, SpaceId, Version};
use net::{
    PageSnapshot, PageUpdate, ServerMessage,
    height::{HeightKey, HeightTile, Replacement},
    light::{self, Header, Update},
};
fn page(x: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y: 0,
        z: 0,
    }
}
#[test]
fn frozen_render_owner_accepts_125_pages_and_1000_light_changes_without_temporal_growth() {
    let mut client = Connection::new(NetworkConfig {
        maximum_pages: 256,
        ..NetworkConfig::default()
    })
    .unwrap();
    for x in 0..125 {
        client.request_page(page(x), 0).unwrap();
    }
    client.take_outgoing(256);
    for x in 0..125 {
        client
            .receive(
                ServerMessage::Page(PageUpdate::Snapshot(PageSnapshot {
                    page: page(x),
                    version: x as u64 + 1,
                    source_version: Version(1),
                    cells: Box::new([Cell::AIR; 4096]),
                })),
                1,
            )
            .unwrap();
        client
            .receive(
                ServerMessage::Light(Box::new(Update::Snapshot(light::Snapshot {
                    header: Header {
                        page: page(x),
                        cell_view: x as u64 + 1,
                        version: 1,
                        source: Version(1),
                    },
                    values: Box::new([0; 4096]),
                }))),
                1,
            )
            .unwrap();
    }
    let mut versions = [1; 125];
    for i in 0..1000 {
        let x = i % 125;
        let version = i as u64 + 2;
        client
            .receive(
                ServerMessage::Light(Box::new(Update::Delta(light::Delta {
                    header: Header {
                        page: page(x as i32),
                        cell_view: x as u64 + 1,
                        version,
                        source: Version(version),
                    },
                    base: versions[x],
                    changes: vec![(i as u16, light::pack(3, 4, [i as u8, 17, 255]).unwrap())],
                }))),
                i as u64 + 2,
            )
            .unwrap();
        versions[x] = version;
    }
    let depth = client.queue_depths();
    assert_eq!(depth.cells, 125);
    assert_eq!(depth.light, 125);
    assert_eq!(
        depth.outgoing, 250,
        "ACKs coalesce by page while the render owner is blocked"
    );
    for _ in 0..125 {
        let update = client.take_light_update().unwrap();
        let image = client.light_replica().page(update.header().page).unwrap();
        let Update::Snapshot(snapshot) = update else {
            panic!("initial image and its deltas compose in place")
        };
        assert_eq!(&snapshot, image);
    }
    assert!(client.take_light_update().is_none());
}
#[test]
fn geometry_and_height_bursts_are_bounded_by_active_subscription_sets() {
    let mut client = Connection::new(NetworkConfig::default()).unwrap();
    for x in 0..128 {
        client.request_geometry(page(x), 0).unwrap();
    }
    client.take_outgoing(256);
    for view in 1..=5 {
        for x in 0..128 {
            client
                .receive(
                    ServerMessage::Geometry(Box::new(net::PublicGeometry {
                        page: page(x),
                        source_version: Version(view),
                        view_version: view,
                        known: [255; 512],
                        occupied: [0; 512],
                        palette: Vec::new(),
                        index_bits: 0,
                        indices: Vec::new(),
                    })),
                    view,
                )
                .unwrap();
        }
    }
    assert_eq!(client.queue_depths().geometry, 128);
    for x in 0..259 {
        let key = HeightKey {
            space: 0,
            level: 4,
            x,
            z: 0,
        };
        client.request_height(key, 0).unwrap();
        if x % 64 == 63 {
            client.take_outgoing(256);
        }
        for source in 1..=3 {
            client
                .receive(
                    ServerMessage::Height(Box::new(HeightTile {
                        key,
                        source,
                        heights: [0; 289],
                        known: [0; 37],
                        atlas: [0; 256],
                        tint: [u32::MAX; 256],
                        protected: [0; 32],
                        replacement: Replacement::None,
                    })),
                    source,
                )
                .unwrap();
        }
    }
    assert_eq!(client.queue_depths().height, 259);
    for _ in 0..259 {
        assert_eq!(client.take_height_update().unwrap().source, 3);
    }
    assert!(client.take_height_update().is_none());
}
