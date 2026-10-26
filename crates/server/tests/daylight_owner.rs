//! Real canonical generation supplies daylight; a persisted high roof survives eviction.
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use net::{ClientMessage, Hello, ServerMessage};
use server::{Server, ServerConfig};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};

const ENTROPY: [u8; 32] = [19; 32];
struct Directory(PathBuf);
impl Directory {
    fn config(&self, feet: BlockPos) -> ServerConfig {
        let mut c = ServerConfig::new(&self.0);
        c.public_seed = 1337;
        c.light_budget = world::lighting::LightBudget::default();
        c.spawn_override = Some([
            f64::from(feet.x) + 0.5,
            f64::from(feet.y),
            f64::from(feet.z) + 0.5,
        ]);
        c
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Harness {
    server: Server,
    now: u64,
}
impl Harness {
    fn step(&mut self) {
        self.now += 10;
        self.server.tick(self.now);
        assert!(!self.server.is_failed(), "{:?}", self.server.counters());
        thread::sleep(Duration::from_micros(300));
    }
    fn until(&mut self, label: &str, mut ready: impl FnMut(&Server) -> bool) {
        let deadline = Instant::now()
            + Duration::from_secs(if label.starts_with("roof darkens") {
                120
            } else {
                40
            });
        while !ready(&self.server) {
            assert!(
                Instant::now() < deadline,
                "{label}: {:?}",
                self.server.counters()
            );
            self.step();
        }
    }
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.server.awaiting_durability() {
            self.server.tick(self.now);
            assert!(!self.server.is_failed());
            assert!(Instant::now() < deadline, "fsync deadline");
            thread::sleep(Duration::from_micros(300));
        }
    }
}
fn sky(server: &Server, p: BlockPos) -> u8 {
    server
        .world()
        .page(p.page())
        .unwrap()
        .light(p.local_index())
        .sky
}
fn roof_pages() -> Vec<PageId> {
    (-1..=1)
        .flat_map(|z| {
            (-1..=1).map(move |x| PageId {
                space: SpaceId(0),
                x,
                y: 16,
                z,
            })
        })
        .collect()
}
#[test]
fn active_surface_gets_real_profile_daylight_and_unloaded_high_roof_stays_dark_after_restart() {
    let directory = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("daylight-owner-{}", std::process::id())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let (store, _) = save::Store::open(&directory.0).unwrap();
    store
        .load_or_create_private_entropy(|| Ok(ENTROPY))
        .unwrap();
    drop(store);
    let generator = gen_authority::ServerGenerator::new(
        r#gen::TerrainSeed(1337),
        gen_authority::PrivateSeed::new(ENTROPY),
    );
    let column = generator.generate_column(0, 0).unwrap();
    let generated: Vec<_> = (0..8)
        .map(|y| {
            column
                .page(
                    PageId {
                        space: SpaceId(0),
                        x: 0,
                        y,
                        z: 0,
                    },
                    None,
                )
                .unwrap()
        })
        .collect();
    // Select an ordinary outdoors surface from actual decorated canonical cells.
    // Its height is only a fixture spawn choice, never a sky certificate.
    let feet = (3..=12)
        .flat_map(|z| (3..=12).map(move |x| (x, z)))
        .find_map(|(x, z)| {
            let y = (0..128).rev().find(|&y| {
                let p = BlockPos {
                    space: SpaceId(0),
                    x,
                    y,
                    z,
                };
                rules::properties(
                    generated[(y / 16) as usize].cells[usize::from(p.local_index().get())],
                )
                .light_opacity
                    > 0
            })? + 1;
            (y < 100).then_some(BlockPos {
                space: SpaceId(0),
                x,
                y,
                z,
            })
        })
        .unwrap();
    let mut world = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(
        feet.page(),
        generated[feet.page().y as usize].cells.to_vec(),
    );
    world.commit(tx).unwrap();
    assert!(
        world
            .page(PageId {
                y: 7,
                ..feet.page()
            })
            .is_none(),
        "no preinstalled source chain"
    );
    let mut h = Harness {
        server: Server::open_with_world(directory.config(feet), world).unwrap(),
        now: 0,
    };
    let ClientMessage::Hello(owner) =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap()
    else {
        panic!("owner")
    };
    let Hello {
        identity,
        name,
        credential,
    } = owner;
    let initial = h.server.handle(
        1,
        ClientMessage::Hello(Hello {
            identity,
            name,
            credential,
        }),
        0,
    );
    let mut welcomed = initial
        .iter()
        .any(|o| matches!(o.message, ServerMessage::Welcome(_)));
    while !welcomed {
        h.now += 10;
        welcomed = h
            .server
            .tick(h.now)
            .iter()
            .any(|o| matches!(o.message, ServerMessage::Welcome(_)));
        thread::sleep(Duration::from_micros(300));
    }
    h.until("canonical outdoor daylight", |s| {
        sky(s, feet) == 15
            && (0..8).all(|y| s.world().page(PageId { y, ..feet.page() }).is_some())
            && s.height_ready_for_publication()
    });
    h.until("initial source frontier settles", |s| {
        let work = s.counters();
        work.light_pending_cells == 0
            && work.light_pending_seed_pages == 0
            && work.light_pending_sky_pages == 0
    });
    h.settle();
    assert_eq!(sky(&h.server, feet), 15);
    assert!(h.server.counters().generation_published >= 7);
    assert!(h.server.counters().light_profile_columns > 0);
    eprintln!(
        "outdoor feet_y={} sky=15 generated={} light_cells={}",
        feet.y,
        h.server.counters().generation_published,
        h.server.counters().light_cells
    );
    // AIR births precede explicit roof edits, preserving their sparse provenance.
    let mut create = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    for p in roof_pages() {
        create.create_page(p, vec![Cell::AIR; 4096]);
    }
    h.server
        .queue_transaction(create)
        .unwrap_or_else(|_| panic!("queue roof birth"));
    h.until("roof pages", |s| {
        roof_pages().iter().all(|p| s.world().page(*p).is_some())
    });
    h.settle();
    let mut roof = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    for z in -16..32 {
        for x in -16..32 {
            roof.set_cell(
                BlockPos {
                    space: SpaceId(0),
                    x,
                    y: 256,
                    z,
                },
                Cell::from_parts(rules::ids::STONE, 0),
            );
        }
    }
    h.server
        .queue_transaction(roof)
        .unwrap_or_else(|_| panic!("queue roof edits"));
    let mut last_diag = 0;
    let start_cells = h.server.counters().light_cells;
    let start_tick = h.server.current_tick().0;
    let mut last_update = (h.server.counters().light_updates, h.server.current_tick().0);
    h.until("roof darkens known surface across unknown gap", |s| {
        let counters = s.counters();
        assert!(
            s.current_tick().0 - start_tick <= 14_000,
            "bounded logical roof phase count"
        );
        assert!(
            counters.light_cells - start_cells <= 12_000_000,
            "bounded9column roof convergence work"
        );
        if counters.light_updates != last_update.0 {
            last_update = (counters.light_updates, s.current_tick().0);
        }
        assert!(
            s.current_tick().0 - last_update.1 <= 1000,
            "lighting must not stall at a full bounded frontier: {counters:?}"
        );
        if s.current_tick().0 >= last_diag + 1000 {
            last_diag = s.current_tick().0;
            eprintln!(
                "roofdiagnostic sky={:?} roof={:?} counters={:?}",
                [feet.y, 95, 111, 127].map(|y| sky(s, BlockPos { y, ..feet })),
                s.world().read(BlockPos { y: 256, ..feet }),
                s.counters()
            );
        }
        sky(s, feet) == 0
    });
    h.settle();
    eprintln!(
        "roof converged logical_ticks={} cells={} sky=0",
        h.server.current_tick().0 - start_tick,
        h.server.counters().light_cells - start_cells
    );
    assert!(
        h.server
            .world()
            .page(PageId {
                y: 8,
                ..feet.page()
            })
            .is_none()
    );
    let mut evict = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    for p in roof_pages() {
        evict.remove_page(p);
    }
    h.server
        .queue_transaction(evict)
        .unwrap_or_else(|_| panic!("queue roof eviction"));
    h.until("roof eviction", |s| {
        roof_pages().iter().all(|p| s.world().page(*p).is_none())
    });
    h.settle();
    assert_eq!(
        sky(&h.server, feet),
        0,
        "eviction does not erase persisted roof provenance"
    );
    h.until("checkpoint admission", |s| !s.awaiting_durability());
    assert!(h.server.request_checkpoint());
    h.settle();
    drop(h);
    let mut recovered = Harness {
        server: Server::open(directory.config(feet)).unwrap(),
        now: 0,
    };
    assert_eq!(sky(&recovered.server, feet), 0);
    assert!(
        roof_pages()
            .iter()
            .all(|p| recovered.server.world().page(*p).is_none())
    );
    for _ in 0..500 {
        recovered.step();
    }
    recovered.settle();
    assert_eq!(
        sky(&recovered.server, feet),
        0,
        "recovered sparse blockers still deny unknown sky"
    );
}
