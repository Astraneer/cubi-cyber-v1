use super::*;
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
fn page(x: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y: 8,
        z: 0,
    }
}
fn position(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn fixture() -> World {
    let mut world = World::new(rules::properties, 64);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in 0..3 {
        let mut cells = vec![Cell::AIR; 4096];
        cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
        // Two actual air rooms, separated by an opaque side wall. The first
        // room sees the front ore plane; the other room cannot see through it.
        for y in 1..16 {
            for lx in 0..16 {
                cells[lx + 16 * 10 + 256 * y] = Cell::from_parts(rules::ids::STONE, 0);
            }
        }
        if x == 2 {
            for y in 1..13 {
                for z in 1..15 {
                    if z != 10 {
                        cells[16 * z + 256 * y] = Cell::from_parts(rules::ids::IRON_ORE, 0);
                    }
                }
            }
        }
        tx.create_page(page(x), cells);
    }
    world.commit(tx).unwrap();
    world
}
#[test]
fn pending_raw_restarts_after_observer_motion_or_changed_known_unknown_ray_sources() {
    for case in ["motion", "wall", "unknown"] {
        let directory = Directory(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/tmp")
                .join(format!("raw-cert-{}-{case}", std::process::id())),
        );
        std::fs::create_dir_all(&directory.0).unwrap();
        let mut config = ServerConfig::new(&directory.0);
        config.generation_enabled = false;
        config.spawn_override = Some([8.5, 129., 8.5]);
        let mut server = Server::open_with_world(config, fixture()).unwrap();
        let hello = net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap())
            .unwrap();
        server.handle(1, hello, 0);
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut now = 0;
        while !server.peers.get(&1).is_some_and(|peer| peer.ready) {
            assert!(Instant::now() < deadline, "owner never ready");
            now += 10;
            server.tick(now);
            assert!(!server.is_failed());
            thread::sleep(Duration::from_micros(300));
        }
        // Exercise the real private owner method without a second simulation
        // owner. Holding this peer outside the map prevents unrelated physics
        // from changing the deliberately selected observation points.
        let mut peer = server.peers.remove(&1).unwrap();
        peer.player.pos = [8.5, 129., 8.5];
        let target = position(32, 129, 8);
        let source = server.raw_sources.source(page(2));
        assert!(
            visibility::target_visible(
                &sim::WorldQuery::new(&server.world, SpaceId(0)),
                target,
                Some(peer.player.eye(1.)),
                &mut visibility::ViewWork::default()
            ),
            "fixture must expose the ore before invalidation"
        );
        peer.raw_view.begin_tick(Tick(99));
        let rays_before = server.counters().visibility_rays;
        let (_, public_air, _) = server
            .build_raw_view(1, &mut peer, page(0), false)
            .expect("public air above the private generator profile must not await LOS");
        assert_eq!(public_air, server.world.page(page(0)).unwrap().cells());
        assert_eq!(server.counters().visibility_rays, rays_before);
        peer.raw_view.begin_tick(Tick(100));
        assert!(
            server
                .build_raw_view(1, &mut peer, page(2), false)
                .is_none()
        );
        let pending = peer.raw_view.pending.as_ref().unwrap();
        assert!(pending.view.next_index() > usize::from(target.local_index().get()));
        assert!(!pending.view.is_complete());
        assert!(pending.dependencies.contains_key(&page(1)));
        match case {
            "motion" => peer.player.pos = [8.5, 129., 12.5],
            "wall" | "unknown" => {
                let mut tx = Transaction::new(Tick(100), SpaceId(0));
                if case == "unknown" {
                    tx.remove_page(page(1));
                } else {
                    for y in 129..144 {
                        for z in 0..10 {
                            tx.set_cell(position(24, y, z), Cell::from_parts(rules::ids::STONE, 0));
                        }
                    }
                }
                let change = server.world.commit(tx).unwrap();
                server.raw_sources.observe(&[change]);
            }
            _ => unreachable!(),
        }
        assert_eq!(
            server.raw_sources.source(page(2)),
            source,
            "target input is unchanged"
        );
        peer.raw_view.begin_tick(Tick(101));
        assert!(
            server
                .build_raw_view(1, &mut peer, page(2), false)
                .is_none(),
            "stale accepted prefix was reused after {case}"
        );
        let mut complete = None;
        for tick in 102..=105 {
            peer.raw_view.begin_tick(Tick(tick));
            complete = server.build_raw_view(1, &mut peer, page(2), false);
            if complete.is_some() {
                break;
            }
        }
        let (_, cells, _) = complete.expect("new observer/ray certificate did not finish");
        assert_eq!(
            cells[usize::from(target.local_index().get())].id(),
            rules::ids::STONE,
            "stale prefix revealed an ore after {case}"
        );
        assert!(server.counters().visibility_source_probes > 0);
    }
}
