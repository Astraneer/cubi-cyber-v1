//! Actual canonical height/geometry/water courier, with WAL-gated edits. Raw
//! pages and private component bytes must never be added by a water subscription.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use gen_authority::{PrivateSeed, ServerGenerator};
use net::{
    ClientMessage, ServerMessage,
    height::{HeightKey, HeightTile, Replacement},
    water::{Request, WaterHalo},
};
use server::{Server, ServerConfig};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{ComponentData, LightSample, Transaction, World};
#[path = "support/exterior.rs"]
mod exterior;
const PRIVATE: [u8; 32] = [37; 32];
const CANARY: &[u8] = b"FAR_WATER_PRIVATE_COMPONENT_MUST_NOT_BE_PUBLISHED";
static NEXT: AtomicU64 = AtomicU64::new(1);
fn key() -> HeightKey {
    HeightKey {
        space: 0,
        level: 0,
        x: -12,
        z: -32,
    }
}
fn page(y: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x: key().x,
        y,
        z: key().z,
    }
}
fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: key().x * 16 + x,
        y,
        z: key().z * 16 + z,
    }
}
fn wi(x: i32, y: i32, z: i32) -> usize {
    (x + 1) as usize + 18 * ((z + 1) as usize + 18 * (y + 1) as usize)
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "far-water-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn config(&self, generation: bool) -> ServerConfig {
        let mut c = ServerConfig::new(&self.0);
        c.public_seed = 1337;
        c.generation_enabled = generation;
        c.spawn_override = Some([
            f64::from(key().x * 16) + 8.5,
            // Keep the cold-projection fixture outside the canonical profile:
            // Hello's legitimate near-spawn requests must not prefill its halo.
            if generation { 200. } else { 100. },
            f64::from(key().z * 16) + 8.5,
        ]);
        c
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(d: &Directory, neighbors: bool) -> World {
    fixture_with_contents(d, neighbors, true)
}
fn fixture_with_contents(d: &Directory, neighbors: bool, contents: bool) -> World {
    let (store, _) = save::Store::open(&d.0).unwrap();
    store
        .load_or_create_private_entropy(|| Ok(PRIVATE))
        .unwrap();
    drop(store);
    let generator = ServerGenerator::new(r#gen::TerrainSeed(1337), PrivateSeed::new(PRIVATE));
    let mut w = World::new(rules::properties, 256);
    let span = if neighbors { -1..=1 } else { 0..=0 };
    for dz in span.clone() {
        for dx in span.clone() {
            let column = generator
                .generate_column(key().x + dx, key().z + dz)
                .unwrap();
            let mut tx = Transaction::new(Tick(0), SpaceId(0));
            for y in 0..8 {
                let id = PageId {
                    x: key().x + dx,
                    z: key().z + dz,
                    ..page(y)
                };
                let cells: Box<[Cell]> = column.page(id, None).unwrap().cells;
                tx.create_page(id, cells);
            }
            w.commit(tx).unwrap();
        }
    }
    if !contents {
        return w;
    }
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(p(8, 62, 8), Cell::from_parts(rules::ids::WATER, 0));
    tx.set_cell(p(8, 63, 8), Cell::AIR);
    for (x, y, z) in [(7, 62, 8), (9, 62, 8), (8, 61, 8), (8, 62, 7), (8, 62, 9)] {
        tx.set_cell(p(x, y, z), Cell::from_parts(rules::ids::STONE, 0));
    }
    tx.set_component(
        ComponentId(900),
        ComponentData {
            kind: 777,
            anchor: p(1, 1, 1),
            payload: CANARY.to_vec(),
        },
        ChannelMask::CONTENTS,
    );
    w.commit(tx).unwrap();
    w
}
struct H {
    server: Server,
    now: u64,
    out: Vec<ServerMessage>,
    geometry: BTreeMap<PageId, net::PublicGeometry>,
}
impl H {
    fn step(&mut self) {
        self.now += 10;
        let out = self.server.tick(self.now);
        self.collect(out);
        assert!(
            !self.server.is_failed(),
            "server failed: {:?}",
            self.server.failure_reason()
        );
        thread::sleep(Duration::from_micros(500));
    }
    fn collect(&mut self, out: Vec<server::Outgoing>) {
        for o in out {
            assert!(
                !matches!(o.message, ServerMessage::Page(_)),
                "water/height requests must not subscribe raw cells"
            );
            let bytes = net::encode_server(&o.message).unwrap();
            assert!(!bytes.windows(PRIVATE.len()).any(|v| v == PRIVATE));
            assert!(!bytes.windows(CANARY.len()).any(|v| v == CANARY));
            let m = net::decode_server(&bytes).unwrap();
            if let ServerMessage::Geometry(g) = &m {
                self.geometry.insert(g.page, (**g).clone());
            }
            self.out.push(m);
        }
    }
    fn send(&mut self, m: ClientMessage) {
        let bytes = net::encode_client(&m).unwrap();
        let m = net::decode_client(&bytes).unwrap();
        let out = self.server.handle(1, m, self.now);
        self.collect(out);
    }
    fn until(&mut self, mut predicate: impl FnMut(&ServerMessage) -> bool) -> ServerMessage {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(i) = self.out.iter().position(&mut predicate) {
                return self.out.remove(i);
            }
            assert!(
                Instant::now() < deadline,
                "water timeout {:?}",
                self.server.far_water_stats()
            );
            self.step();
        }
    }
    fn ticks(&mut self, n: u64) {
        let target = self.server.current_tick().0 + n;
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.server.current_tick().0 < target || self.server.awaiting_durability() {
            assert!(Instant::now() < deadline);
            self.step();
        }
    }
    fn exact(&mut self) -> HeightTile {
        self.out
            .retain(|m| !matches!(m,ServerMessage::Height(tile) if tile.key==key()));
        self.send(ClientMessage::RequestExactHeight {
            key: key(),
            known: None,
        });
        let m = self.until(|m| matches!(m,ServerMessage::Height(t)if t.key==key() && matches!(t.replacement,Replacement::Column {..})));
        let ServerMessage::Height(tile) = m else {
            unreachable!()
        };
        let Replacement::Column {
            geometry,
            min_y,
            max_y,
        } = &tile.replacement
        else {
            panic!("real column required")
        };
        assert_eq!((*min_y, *max_y, geometry.len()), (0, 7, 8));
        for stamp in geometry {
            let actual = self.geometry.get(&stamp.page).unwrap();
            assert_eq!(
                (stamp.source, stamp.view),
                (actual.source_version, actual.view_version)
            );
        }
        *tile
    }
    fn water(&mut self) -> Box<WaterHalo> {
        self.water_at(page(3))
    }
    fn water_at(&mut self, page: PageId) -> Box<WaterHalo> {
        let m = self.until(|m| matches!(m,ServerMessage::WaterHalo(w)if w.page==page));
        let ServerMessage::WaterHalo(w) = m else {
            unreachable!()
        };
        w
    }
}
fn open(d: &Directory, neighbors: bool, generation: bool) -> H {
    open_world(d, fixture(d, neighbors), generation)
}
fn open_world(d: &Directory, world: World, generation: bool) -> H {
    let mut h = H {
        server: Server::open_with_world(d.config(generation), world).unwrap(),
        now: 0,
        out: vec![],
        geometry: BTreeMap::new(),
    };
    let ClientMessage::Hello(hello) =
        net::decode_client(&std::fs::read(d.0.join("owner.identity")).unwrap()).unwrap()
    else {
        panic!("owner")
    };
    h.send(ClientMessage::Hello(hello));
    h.until(|m| matches!(m, ServerMessage::Welcome(_)));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !h.server.height_ready_for_publication() || h.server.awaiting_durability() {
        assert!(Instant::now() < deadline);
        h.step();
    }
    h
}
fn assert_halo(water: &WaterHalo, reference: &World) {
    for y in -1..=16 {
        for z in -1..=16 {
            for x in -1..=16 {
                let read = reference.read(p(x, water.page.y * 16 + y, z));
                let expected = match read {
                    world::CellRead::Unknown => 0,
                    world::CellRead::Known(cell) if cell.id() == rules::ids::WATER => {
                        if cell.state() & 15 < 8 {
                            10 - (cell.state() & 15) as u8
                        } else {
                            18
                        }
                    }
                    world::CellRead::Known(cell) if rules::properties(cell).opaque => 2,
                    world::CellRead::Known(_) => 1,
                };
                assert_eq!(water.cells[wi(x, y, z)], expected);
            }
        }
    }
}
fn assert_geometry(h: &H, reference: &World) {
    let terrain = r#gen::Generator::new(r#gen::TerrainSeed(1337));
    let tints = std::array::from_fn(|i| {
        terrain
            .column_info(
                key().x * 16 + (i % 16) as i32,
                key().z * 16 + (i / 16) as i32,
            )
            .tint
    });
    for y in 0..8 {
        let actual = &h.geometry[&page(y)];
        let expected =
            server::lod::geometry_from_page(reference.page(page(y)).unwrap(), Some(&tints));
        assert_eq!(actual.known, expected.known);
        assert_eq!(actual.occupied, expected.occupied);
        assert_eq!(actual.index_bits, expected.index_bits);
        assert_eq!(actual.indices, expected.indices);
        assert_eq!(actual.palette.len(), expected.palette.len());
        for (a, b) in actual.palette.iter().zip(expected.palette) {
            assert_eq!((a.atlas_layer, a.tint), (b.atlas_layer, b.tint));
            assert_eq!(
                a.kind,
                match b.kind {
                    server::lod::PublicSurfaceKind::Opaque => net::PublicSurfaceKind::Opaque,
                    server::lod::PublicSurfaceKind::Cutout => net::PublicSurfaceKind::Cutout,
                    server::lod::PublicSurfaceKind::Water => net::PublicSurfaceKind::Water,
                }
            );
        }
    }
}
fn request(h: &H, tile: &HeightTile) -> Request {
    let g = &h.geometry[&page(3)];
    Request {
        page: page(3),
        geometry_source: g.source_version,
        geometry_view: g.view_version,
        column_source: tile.source,
        acknowledged: None,
    }
}
#[test]
fn actual_column_water_is_private_wal_gated_credit_bounded_and_neighbor_versioned() {
    let d = Directory::new();
    let mut h = open(&d, true, false);
    let tile = h.exact();
    let req = request(&h, &tile);
    h.send(ClientMessage::RequestWater(Request {
        geometry_view: req.geometry_view + 1,
        ..req
    }));
    h.until(|m| matches!(m,ServerMessage::WaterUnavailable(p)if *p==page(3)));
    assert_eq!(h.server.far_water_stats().streams, 0);
    h.send(ClientMessage::RequestWater(req));
    let first = h.water();
    assert_eq!(
        (
            first.geometry_source,
            first.geometry_view,
            first.column_source
        ),
        (req.geometry_source, req.geometry_view, req.column_source)
    );
    assert!(!first.cells.contains(&0));
    assert_eq!(first.cells[wi(8, 14, 8)], 10);
    let initial = h.server.far_water_stats();
    assert_eq!(
        (initial.published, initial.streams, initial.watcher_links),
        (1, 1, 27)
    );
    h.out.clear();
    h.ticks(4);
    assert_eq!(h.server.far_water_stats().submitted, initial.submitted);
    let mut tx = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    tx.set_light(
        p(-1, 49, 8),
        LightSample {
            sky: 15,
            block: 7,
            rgb: [1, 2, 3],
        },
    );
    assert!(h.server.queue_transaction(tx).is_ok());
    h.ticks(4);
    assert_eq!(
        h.server.far_water_stats().submitted,
        initial.submitted,
        "LIGHT must never schedule water"
    );
    let before = h.server.world().version();
    let mut tx = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    // A real accepted portal connects this halo sample to known exterior.
    for y in 49..128 {
        tx.set_cell(p(-1, y, 8), Cell::AIR);
    }
    assert!(h.server.queue_transaction(tx).is_ok());
    h.ticks(4);
    assert!(h.server.world().version() > before);
    assert_eq!(
        h.server.world().read(p(-1, 49, 8)),
        world::CellRead::Known(Cell::AIR)
    );
    assert!(
        !h.out
            .iter()
            .any(|m| matches!(m, ServerMessage::WaterHalo(_))),
        "no automatic second full image before ACK"
    );
    // The portal may expose other vertical leaves too. Their changed public
    // geometry updates the whole-column manifest even if this centre leaf is
    // unchanged, so a former column binding cannot authorize the new image.
    let current_tile = h.exact();
    let current = request(&h, &current_tile);
    h.send(ClientMessage::RequestWater(Request {
        acknowledged: Some(first.water_view),
        ..current
    }));
    let changed = h.water();
    assert!(changed.water_view > first.water_view);
    assert_eq!(
        (
            changed.geometry_source,
            changed.geometry_view,
            changed.column_source
        ),
        (
            current.geometry_source,
            current.geometry_view,
            current.column_source
        )
    );
    assert_eq!(changed.cells[wi(-1, 1, 8)], 1);
    assert_eq!(first.cells[wi(-1, 1, 8)], 2);
    h.send(ClientMessage::ForgetWater(page(3)));
    h.ticks(1);
    assert_eq!(h.server.far_water_stats().streams, 0);
    assert_eq!(h.server.far_water_stats().watcher_links, 0);
    let last = h.server.far_water_stats().submitted;
    h.ticks(4);
    assert_eq!(h.server.far_water_stats().submitted, last);
}
#[test]
fn cold_canonical_halo_matches_private_authority_without_creating_gameplay_pages() {
    let d = Directory::new();
    let reference = fixture(&d, true);
    let reference = exterior::sealed_world(
        &reference,
        &r#gen::Generator::new(r#gen::TerrainSeed(1337)),
        page(0),
    );
    let mut h = open(&d, false, true);
    let before_pages = h.server.world().pages().count();
    let before_generation = h.server.counters().generation_submitted;
    assert_eq!(before_pages, 8);
    let tile = h.exact();
    let req = request(&h, &tile);
    h.send(ClientMessage::RequestWater(req));
    let first = h.water();
    assert!(!first.cells.contains(&0));
    assert_eq!(first.cells[wi(8, 14, 8)], 10);
    assert_halo(&first, &reference);
    assert_geometry(&h, &reference);
    // ACKing a current immutable result must not launch a legacy capture or
    // schedule canonical generation just to fill its far-only halo.
    h.send(ClientMessage::RequestWater(Request {
        acknowledged: Some(first.water_view),
        ..req
    }));
    h.ticks(4);
    assert!(
        !h.out
            .iter()
            .any(|m| matches!(m, ServerMessage::WaterHalo(_)))
    );
    assert_eq!(h.server.world().pages().count(), before_pages);
    assert_eq!(h.server.counters().generation_submitted, before_generation);
    let stats = h.server.far_water_stats();
    assert_eq!(stats.submitted, 0);
    assert_eq!(stats.canonical_generation_attempts, 0);
    assert_eq!(stats.projected_images, 1);
    let cold = h.server.cold_projection_stats().unwrap();
    assert_eq!(
        (cold.heads, cold.leases, cold.running, cold.queued),
        (8, 8, 0, 0)
    );
    eprintln!(
        "farWater cold streams{} projectedImages{} legacyJobs{} canonicalGeneration{} retainedWorldpages{} cacheBytes{}",
        stats.streams,
        stats.projected_images,
        stats.submitted,
        h.server.counters().generation_submitted,
        h.server.world().pages().count(),
        cold.cache_bytes
    );
}
#[test]
fn entirely_cold_exact_column_and_all_halos_leave_world_and_light_empty() {
    let d = Directory::new();
    let reference = fixture_with_contents(&d, true, false);
    let reference = exterior::sealed_world(
        &reference,
        &r#gen::Generator::new(r#gen::TerrainSeed(1337)),
        page(0),
    );
    let mut h = open_world(&d, World::new(rules::properties, 256), true);
    assert_eq!(h.server.world().pages().count(), 0);
    let before_version = h.server.world().version();
    let tile = h.exact();
    assert_geometry(&h, &reference);
    for y in 0..8 {
        let geometry = &h.geometry[&page(y)];
        let request = Request {
            page: page(y),
            geometry_source: geometry.source_version,
            geometry_view: geometry.view_version,
            column_source: tile.source,
            acknowledged: None,
        };
        h.send(ClientMessage::RequestWater(request));
        let water = h.water_at(page(y));
        assert_eq!(
            (
                water.geometry_source,
                water.geometry_view,
                water.column_source
            ),
            (request.geometry_source, request.geometry_view, tile.source)
        );
        assert_halo(&water, &reference);
        if (1..7).contains(&y) {
            assert!(!water.cells.contains(&0));
        } else {
            assert!(
                water.cells.contains(&0),
                "missing out-of-profile inputs stay Unknown"
            );
        }
        h.send(ClientMessage::RequestWater(Request {
            acknowledged: Some(water.water_view),
            ..request
        }));
    }
    h.ticks(4);
    assert_eq!(h.server.world().pages().count(), 0);
    assert_eq!(h.server.world().version(), before_version);
    let counters = h.server.counters();
    assert_eq!(counters.generation_submitted, 0);
    assert_eq!((counters.light_cells, counters.light_updates), (0, 0));
    assert_eq!(
        (
            counters.light_pending_seed_pages,
            counters.light_pending_bootstrap_pages
        ),
        (0, 0)
    );
    let water = h.server.far_water_stats();
    assert_eq!(
        (
            water.projected_images,
            water.submitted,
            water.canonical_generation_attempts
        ),
        (8, 0, 0)
    );
    let cold = h.server.cold_projection_stats().unwrap();
    assert_eq!(
        (cold.heads, cold.cache_entries, cold.running, cold.queued),
        (8, 1, 0, 0)
    );
    eprintln!(
        "farWater entirelyCold geometry8 halos8 worldPages0 canonicalGeneration0 lightSteps0 cacheBytes{}",
        cold.cache_bytes
    );
}
