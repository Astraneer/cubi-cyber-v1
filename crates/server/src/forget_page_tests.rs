use super::*;
use net::PageUpdate;
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn page(x: i32, y: i32, z: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}

fn await_raw_page(server: &mut Server, now: &mut u64, page: PageId) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "replacement raw lease never published"
        );
        *now += 10;
        let output = server.tick(*now);
        assert!(!server.is_failed());
        if let Some(version) = output.iter().find_map(|out| match &out.message {
            ServerMessage::Page(PageUpdate::Snapshot(p))
                if out.connection == 1 && p.page == page =>
            {
                Some(p.version)
            }
            ServerMessage::Page(PageUpdate::Delta(p)) if out.connection == 1 && p.page == page => {
                Some(p.version)
            }
            _ => None,
        }) {
            return version;
        }
        thread::sleep(Duration::from_micros(300));
    }
}

#[test]
fn forget_page_releases_full_near_credit_pending_views_and_dependent_streams_without_world_changes()
{
    let directory = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("forget-page-{}", std::process::id())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let retired = page(0, 9, 0);
    let desired = page(0, 8, 0);
    let mut world = World::new(rules::properties, 4096);
    let mut transaction = world::Transaction::new(Tick(1), SpaceId(0));
    let mut leases = Vec::new();
    for y in 5..=11 {
        for z in -3..=3 {
            for x in -3..=3 {
                let p = page(x, y, z);
                let cell = if p == retired {
                    Cell::from_parts(rules::ids::IRON_ORE, 0)
                } else {
                    Cell::AIR
                };
                let mut cells = vec![cell; 4096];
                if y == 8 {
                    cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
                }
                transaction.create_page(p, cells);
                if p != desired && leases.len() < MAX_AOI_PAGES {
                    leases.push(p);
                }
            }
        }
    }
    assert!(leases.contains(&retired));
    world.commit(transaction).unwrap();
    let mut config = ServerConfig::new(&directory.0);
    config.generation_enabled = false;
    config.spawn_override = Some([8.5, 129., 8.5]);
    let mut server = Server::open_with_world(config, world).unwrap();
    let hello =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap();
    server.handle(1, hello, 0);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut now = 0;
    while !server.peers.get(&1).is_some_and(|peer| peer.ready) {
        assert!(Instant::now() < deadline, "owner handshake deadline");
        now += 10;
        server.tick(now);
        thread::sleep(Duration::from_micros(300));
    }
    server.peers.get_mut(&1).unwrap().pages = leases.into_iter().collect();
    let mut batch = Batch::empty(server.tick);
    let mut outgoing = Vec::new();
    server.process_message(
        1,
        ClientMessage::RequestPage {
            page: desired,
            acknowledged: None,
        },
        now,
        &mut batch,
        &mut outgoing,
    );
    assert!(
        server.publish_pages().iter().any(|out| out.connection == 1
            && matches!(out.message,ServerMessage::PageUnavailable(p) if p==desired)),
        "negative control must exhaust the unchanged256-page lease limit"
    );

    let mut peer = server.peers.remove(&1).unwrap();
    peer.raw_view.begin_tick(Tick(server.tick.0 + 1));
    assert!(
        server
            .build_raw_view(1, &mut peer, retired, false)
            .is_none()
    );
    assert_eq!(
        peer.raw_view.pending_request().map(|(p, _)| p),
        Some(retired)
    );
    peer.history
        .build(
            retired,
            server.world.version(),
            Box::new([Cell::from_parts(rules::ids::STONE, 0); 4096]),
            false,
        )
        .unwrap()
        .unwrap();
    let version = peer.history.latest_page(retired).unwrap().version;
    peer.history.acknowledge(retired, version);
    peer.raw_view.admit(
        retired,
        version,
        server.raw_sources.source(retired).unwrap(),
    );
    peer.page_requests
        .extend([(retired, false), (retired, true)]);
    peer.summaries.push_back(retired);
    server.peers.insert(1, peer);
    let head = Arc::new(Frame {
        page: retired,
        cell_view: version,
        view: 7,
        source: Version(7),
        tick: server.tick,
        hints: vec![
            net::props::Hint::from_visual(
                core_types::LocalIdx::new(0).unwrap(),
                net::props::Kind::Chest,
                None,
                false,
                Some(0.5),
                None,
            )
            .unwrap(),
        ],
    });
    for connection in [1, 2] {
        let state = server.props_stream.peers.entry(connection).or_default();
        state.heads.insert(retired, Arc::clone(&head));
        state.hints = 1;
        state.next = 7;
        state.queue.push_back((retired, true));
    }
    server.light_stream.request(
        1,
        net::light::Request {
            page: retired,
            cell_view: version,
            acknowledged: None,
        },
    );
    server.packet_stream.queue(1, retired, true, server.tick);
    let world_version = server.world.version();
    let pages = server.world.loaded_pages();
    let original = server.world.page(retired).unwrap().cells();
    let generation = server.counters.generation_submitted;
    for _ in 0..2 {
        server.process_message(
            1,
            ClientMessage::ForgetPage(retired),
            now,
            &mut batch,
            &mut outgoing,
        );
        let peer = &server.peers[&1];
        assert_eq!(peer.pages.len(), MAX_AOI_PAGES - 1);
        assert!(!peer.pages.contains(&retired));
        assert!(peer.history.latest_page(retired).is_none());
        assert!(peer.raw_view.pending_request().is_none());
        assert!(
            !peer
                .raw_view
                .validates(retired, version, server.raw_sources.source(retired))
        );
        assert!(peer.page_requests.iter().all(|(p, _)| *p != retired));
        assert!(!peer.summaries.contains(&retired));
        let props = &server.props_stream.peers[&1];
        assert_eq!((props.hints, props.next), (0, 7));
        assert!(!props.heads.contains_key(&retired));
        assert!(props.queue.is_empty());
        assert_eq!(server.props_stream.peers[&2].hints, 1);
        assert!(server.props_stream.peers[&2].heads.contains_key(&retired));
        assert_eq!(server.world.version(), world_version);
        assert_eq!(server.world.loaded_pages(), pages);
        assert_eq!(server.world.page(retired).unwrap().cells(), original);
        assert_eq!(server.counters.generation_submitted, generation);
        assert_eq!(
            outgoing
                .iter()
                .filter(|out| out.connection == 1
                    && matches!(out.message,ServerMessage::PageUnavailable(p) if p==retired))
                .count(),
            1,
            "one retirement echo, no second response for the idempotent duplicate"
        );
    }
    assert!(server.publish_lights().is_empty());
    assert!(server.publish_packets().is_empty());
    server.process_message(
        1,
        ClientMessage::RequestPage {
            page: desired,
            acknowledged: None,
        },
        now,
        &mut batch,
        &mut outgoing,
    );
    let renewed = await_raw_page(&mut server, &mut now, desired);
    assert_eq!(server.peers[&1].pages.len(), MAX_AOI_PAGES);
    assert!(server.peers[&1].pages.contains(&desired));
    assert!(!server.peers[&1].pages.contains(&retired));
    assert_eq!(server.counters.generation_submitted, generation);
    // A prior in-flight Forget may complete after the camera already renewed
    // this page. The explicit echo supplies repair credit instead of leaving a
    // received=true client permanently subscribed to a removed server lease.
    outgoing.clear();
    server.process_message(
        1,
        ClientMessage::ForgetPage(desired),
        now,
        &mut batch,
        &mut outgoing,
    );
    assert!(outgoing.iter().any(|out| out.connection == 1
        && matches!(out.message,ServerMessage::PageUnavailable(p) if p==desired)));
    server.process_message(
        1,
        ClientMessage::RequestPage {
            page: desired,
            acknowledged: None,
        },
        now,
        &mut batch,
        &mut outgoing,
    );
    let repaired = await_raw_page(&mut server, &mut now, desired);
    assert!(repaired > renewed);
    assert!(server.peers[&1].pages.contains(&desired));
    assert_eq!(server.peers[&1].pages.len(), MAX_AOI_PAGES);
    assert_eq!(server.counters.generation_submitted, generation);
}
