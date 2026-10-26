//! Ordering between the bounded network cache and the slower rendering owner.
use client::{
    ClientScene, SceneConfig,
    network::{Connection, NetworkConfig},
    remote::RemoteSession,
};
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, Version};
use net::{
    ClientMessage, PageSnapshot, PageUpdate, ServerMessage,
    light::{Header, Snapshot, Update},
};
fn page() -> PageId {
    PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    }
}
fn raw(view: u64) -> PageUpdate {
    PageUpdate::Snapshot(PageSnapshot {
        page: page(),
        version: view,
        source_version: Version(view),
        cells: Box::new([Cell::AIR; 4096]),
    })
}
fn light(cells: u64, view: u64) -> ServerMessage {
    ServerMessage::Light(Box::new(Update::Snapshot(Snapshot {
        header: Header {
            page: page(),
            cell_view: cells,
            version: view,
            source: Version(1),
        },
        values: Box::new([0; 4096]),
    })))
}
#[test]
fn unavailable_revokes_queued_raw_and_light_before_the_slow_owner_can_resurrect_them() {
    let mut c = Connection::new(NetworkConfig::default()).unwrap();
    c.request_page(page(), 0).unwrap();
    c.take_outgoing(256);
    c.receive(ServerMessage::Page(raw(1)), 1).unwrap();
    c.receive(light(1, 1), 2).unwrap();
    c.receive(ServerMessage::PageUnavailable(page()), 3)
        .unwrap();
    assert!(c.replica().page(page()).is_none());
    assert!(c.light_replica().page(page()).is_none());
    assert!(c.take_page_updates(8).is_empty());
    assert!(c.take_light_update().is_none());
    assert_eq!(c.take_page_removal(), Some(page()));
    assert!(!c.take_outgoing(256).iter().any(|m| matches!(m,
        ClientMessage::AckPage{page:p,..} if *p==page())
        || matches!(m,ClientMessage::RequestLight(r)if r.page==page())));
    c.receive(ServerMessage::Page(raw(2)), 4).unwrap();
    assert!(matches!(c.take_page_updates(1).as_slice(),[PageUpdate::Snapshot(p)]if p.version==2));
}
#[test]
fn raw_binding_change_requests_light_fallback_instead_of_acknowledging_an_obsolete_binding() {
    let mut c = Connection::new(NetworkConfig::default()).unwrap();
    c.request_page(page(), 0).unwrap();
    c.take_outgoing(256);
    c.receive(ServerMessage::Page(raw(1)), 1).unwrap();
    c.receive(light(1, 1), 2).unwrap();
    c.take_outgoing(256);
    // Future LIGHT is unapplicable until its corresponding raw image arrives.
    c.receive(light(2, 2), 3).unwrap();
    c.receive(ServerMessage::Page(raw(2)), 4).unwrap();
    assert!(
        c.take_outgoing(256).iter().any(|m| matches!(m,
        ClientMessage::RequestLight(r)if r.page==page()&&r.cell_view==2&&r.acknowledged.is_none()))
    );
    c.receive(light(2, 2), 5).unwrap();
    assert_eq!(c.light_replica().page(page()).unwrap().header.cell_view, 2);
}
#[test]
fn public_binding_diagnostics_distinguish_retained_light_from_a_current_certificate() {
    let mut session = RemoteSession::new(NetworkConfig::default()).unwrap();
    let mut scene = ClientScene::new(SceneConfig {
        authoritative_remote: true,
        center: BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 8,
            z: 8,
        },
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 27,
        ..SceneConfig::forest()
    })
    .unwrap();
    session.connection_mut().request_page(page(), 0).unwrap();
    session
        .connection_mut()
        .receive(ServerMessage::Page(raw(1)), 1)
        .unwrap();
    session.connection_mut().receive(light(1, 1), 2).unwrap();
    scene.import_authoritative_page(&raw(1), Tick(1)).unwrap();
    let ServerMessage::Light(l) = light(1, 1) else {
        unreachable!()
    };
    scene.import_authoritative_light(&l, Tick(1)).unwrap();
    assert!(session.pending_light_bindings(&scene).next().is_none());
    scene.import_authoritative_page(&raw(2), Tick(2)).unwrap();
    let d = session.pending_light_bindings(&scene).next().unwrap();
    assert_eq!(d.page, page());
    assert_eq!(d.scene_cells, Some(2));
    assert_eq!(d.network_cells, Some(1));
    assert_eq!(d.network_light.unwrap().cell_view, 1);
    assert_eq!(d.retained_scene_light.unwrap().cell_view, 1);
    assert!(scene.authoritative_light_version(page()).is_none());
}
#[test]
fn measured_368_public_pages_fit_the_bounded_512_geometry_interest() {
    let mut c = Connection::new(NetworkConfig::default()).unwrap();
    let mut queued = 0;
    for x in 0..net::MAX_PUBLIC_GEOMETRY_PAGES {
        c.request_geometry(
            PageId {
                x: x as i32,
                ..page()
            },
            0,
        )
        .unwrap();
        if (x + 1) % 128 == 0 {
            queued += c.take_outgoing(256).len();
        }
    }
    assert!(c.request_geometry(PageId { x: 512, ..page() }, 0).is_err());
    queued += c.take_outgoing(256).len();
    assert_eq!(queued, 512);
}
