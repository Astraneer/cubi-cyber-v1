//! Real encoded courier differential: an enclosed canonical private component
//! and the same world filled with the public baseline are indistinguishable.
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, Version};
use net::{
    ClientMessage, ServerMessage,
    height::{HeightKey, HeightTile},
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
use world::{CellRead, Transaction, World};
#[path = "support/private_pocket.rs"]
mod pocket;
use pocket::worlds;
static NEXT: AtomicU64 = AtomicU64::new(1);
fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn page(y: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x: 1,
        y,
        z: 0,
    }
}
fn key() -> HeightKey {
    HeightKey {
        space: 0,
        level: 0,
        x: 1,
        z: 0,
    }
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "private-exterior-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct H {
    server: Server,
    now: u64,
    out: Vec<ServerMessage>,
    geometry: BTreeMap<PageId, net::PublicGeometry>,
}
impl H {
    fn open(d: &Directory, world: World) -> Self {
        let (store, _) = save::Store::open(&d.0).unwrap();
        store
            .load_or_create_private_entropy(|| Ok([37; 32]))
            .unwrap();
        drop(store);
        let mut c = ServerConfig::new(&d.0);
        c.public_seed = 1;
        c.spawn_override = Some([24.5, 200., 8.5]);
        let mut h = Self {
            server: Server::open_with_world(c, world).unwrap(),
            now: 0,
            out: Vec::new(),
            geometry: BTreeMap::new(),
        };
        let ClientMessage::Hello(hello) =
            net::decode_client(&std::fs::read(d.0.join("owner.identity")).unwrap()).unwrap()
        else {
            panic!("owner")
        };
        h.send(ClientMessage::Hello(hello));
        let deadline = Instant::now() + Duration::from_secs(40);
        while !h.server.height_ready_for_publication() || h.server.awaiting_durability() {
            assert!(Instant::now() < deadline);
            h.step()
        }
        h
    }
    fn collect(&mut self, out: Vec<server::Outgoing>) {
        for o in out {
            assert!(
                !matches!(o.message, ServerMessage::Page(_)),
                "no near visibility subscription exists"
            );
            let wire = net::encode_server(&o.message).unwrap();
            let m = net::decode_server(&wire).unwrap();
            if let ServerMessage::Geometry(g) = &m {
                self.geometry.insert(g.page, (**g).clone());
            }
            self.out.push(m)
        }
    }
    fn send(&mut self, m: ClientMessage) {
        let wire = net::encode_client(&m).unwrap();
        let out = self
            .server
            .handle(1, net::decode_client(&wire).unwrap(), self.now);
        self.collect(out)
    }
    fn step(&mut self) {
        self.now += 10;
        let out = self.server.tick(self.now);
        self.collect(out);
        assert!(
            !self.server.is_failed(),
            "{:?}",
            self.server.failure_reason()
        );
        thread::sleep(Duration::from_micros(500))
    }
    fn until(&mut self, mut predicate: impl FnMut(&ServerMessage) -> bool) -> ServerMessage {
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            if let Some(i) = self.out.iter().position(&mut predicate) {
                return self.out.remove(i);
            }
            assert!(Instant::now() < deadline);
            self.step()
        }
    }
    fn exact(&mut self) -> Box<HeightTile> {
        self.send(ClientMessage::RequestExactHeight {
            key: key(),
            known: None,
        });
        let ServerMessage::Height(t) =
            self.until(|m| matches!(m,ServerMessage::Height(t)if t.key==key()))
        else {
            unreachable!()
        };
        assert_eq!(self.geometry.len(), 8);
        t
    }
    fn water(&mut self, t: &HeightTile, y: i32) -> Box<WaterHalo> {
        let g = &self.geometry[&page(y)];
        self.send(ClientMessage::RequestWater(Request {
            page: page(y),
            geometry_source: g.source_version,
            geometry_view: g.view_version,
            column_source: t.source,
            acknowledged: None,
        }));
        let ServerMessage::WaterHalo(w) =
            self.until(|m| matches!(m,ServerMessage::WaterHalo(w)if w.page==page(y)))
        else {
            unreachable!()
        };
        w
    }
}
fn normalized_geometry(mut g: net::PublicGeometry) -> Vec<u8> {
    g.source_version = Version(1);
    g.view_version = 1;
    net::encode_server(&ServerMessage::Geometry(Box::new(g))).unwrap()
}
fn normalized_water(mut w: Box<WaterHalo>) -> Vec<u8> {
    w.geometry_source = Version(1);
    w.geometry_view = 1;
    w.water_view = 1;
    w.column_source = 1;
    net::encode_server(&ServerMessage::WaterHalo(w)).unwrap()
}
fn normalized_height(mut tile: Box<HeightTile>) -> Vec<u8> {
    // Independent server instances allocate unrelated global public IDs.
    tile.source = 1;
    if let net::height::Replacement::Column { geometry, .. } = &mut tile.replacement {
        for stamp in geometry {
            stamp.source = Version(1);
            stamp.view = 1;
        }
    }
    // Protection/known/error/replacement bytes remain unchanged.
    net::encode_server(&ServerMessage::Height(tile)).unwrap()
}
fn halo_index(p: BlockPos, page: PageId) -> usize {
    (p.x - page.x * 16 + 1) as usize
        + 18 * ((p.z - page.z * 16 + 1) as usize + 18 * (p.y - page.y * 16 + 1) as usize)
}
#[test]
fn encoded_closed_private_air_lava_equal_public_filled_component_and_real_portal_reveals_them() {
    let (aw, bw, component) = worlds();
    let da = Directory::new();
    let db = Directory::new();
    let mut a = H::open(&da, aw);
    let mut b = H::open(&db, bw);
    let ta = a.exact();
    let tb = b.exact();

    assert_eq!(
        normalized_height(ta.clone()),
        normalized_height(tb.clone()),
        "closed private component cannot change HEIGHT/protection/replacement or its public source"
    );
    for y in 0..8 {
        assert_eq!(
            normalized_geometry(a.geometry[&page(y)].clone()),
            normalized_geometry(b.geometry[&page(y)].clone()),
            "closed component changes no encoded geometry body"
        );
    }
    let wa = a.water(&ta, 2);
    let wb = b.water(&tb, 2);
    assert_eq!(normalized_water(wa.clone()), normalized_water(wb));
    let sample = pos(21, 35, 3);
    assert_eq!(wa.cells[halo_index(sample, page(2))], 2);
    assert_eq!(
        a.server.world().read(sample),
        CellRead::Known(Cell::AIR),
        "privacy must not rewrite authority"
    );
    let lava = *component
        .iter()
        .find(|p| {
            a.server.world().read(**p) == CellRead::Known(Cell::from_parts(rules::ids::LAVA, 0))
        })
        .unwrap();
    assert_eq!(
        a.server.world().read(lava),
        CellRead::Known(Cell::from_parts(rules::ids::LAVA, 0))
    );
    let old_head = a.geometry[&page(2)].source_version;
    let mut tx = Transaction::new(Tick(a.server.current_tick().0 + 1), SpaceId(0));
    for y in 35..128 {
        tx.set_cell(pos(21, y, 3), Cell::AIR);
    }
    assert!(a.server.queue_transaction(tx).is_ok());
    let before = a.server.world().version();
    let deadline = Instant::now() + Duration::from_secs(40);
    while a.server.world().version() <= before || a.server.awaiting_durability() {
        assert!(Instant::now() < deadline);
        a.step()
    }
    a.send(ClientMessage::ForgetWater(page(2)));
    a.out
        .retain(|m| !matches!(m, ServerMessage::Height(_) | ServerMessage::WaterHalo(_)));
    let opened = a.exact();
    let water = a.water(&opened, 2);
    assert!(a.geometry[&page(2)].source_version > old_head);
    assert!(water.water_view > wa.water_view);
    assert_eq!(water.cells[halo_index(sample, page(2))], 1);
    let i = usize::from(sample.local_index().get());
    assert_eq!(a.geometry[&page(2)].occupied[i / 8] >> (i % 8) & 1, 0);
    assert_eq!(
        a.server.world().read(lava),
        CellRead::Known(Cell::from_parts(rules::ids::LAVA, 0)),
        "opening changes public exposure while canonical lava survives"
    );
}

#[test]
fn neighbouring_portal_automatically_republishes_centre_geometry_and_public_height_manifest() {
    let mut world = World::new(rules::properties, 64);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in 1..3 {
        for y in 0..8 {
            tx.create_page(
                PageId { x, ..page(y) },
                vec![Cell::from_parts(rules::ids::STONE, 0); 4096],
            );
        }
    }
    for x in 31..34 {
        tx.set_cell(pos(x, 40, 8), Cell::AIR);
    }
    world.commit(tx).unwrap();
    let directory = Directory::new();
    let mut h = H::open(&directory, world);
    let old = h.exact();
    let old_geometry = h.geometry[&page(2)].clone();
    let water = h.water(&old, 2);
    h.send(ClientMessage::RequestWater(Request {
        page: page(2),
        geometry_source: water.geometry_source,
        geometry_view: water.geometry_view,
        column_source: water.column_source,
        acknowledged: Some(water.water_view),
    }));
    let tick = h.server.current_tick().0 + 1;
    while h.server.current_tick().0 < tick || h.server.awaiting_durability() {
        h.step()
    }
    h.out.retain(|m| {
        !matches!(
            m,
            ServerMessage::Height(_) | ServerMessage::Geometry(_) | ServerMessage::WaterHalo(_)
        )
    });
    let centre_version = h
        .server
        .world()
        .input_version(page(2), server::far_water::capture::CHANNELS);
    let old_world = h.server.world().version();
    let mut tx = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    for y in 40..128 {
        tx.set_cell(pos(33, y, 8), Cell::AIR);
    }
    assert!(h.server.queue_transaction(tx).is_ok());
    let deadline = Instant::now() + Duration::from_secs(40);
    while h.server.world().version() == old_world || h.server.awaiting_durability() {
        assert!(Instant::now() < deadline);
        h.step()
    }
    assert_eq!(
        h.server
            .world()
            .input_version(page(2), server::far_water::capture::CHANNELS),
        centre_version,
        "centre input itself did not change"
    );
    // There is intentionally no new RequestGeometry/RequestHeight here. Only
    // the certified derived-head wake can republish the unchanged-HID column.
    let ServerMessage::Height(updated) =
        h.until(|m| matches!(m,ServerMessage::Height(t)if t.key==key()&&t.source>old.source))
    else {
        unreachable!()
    };
    assert!(h.geometry[&page(2)].source_version > old_geometry.source_version);
    assert_eq!(updated.heights, old.heights);
    assert_eq!(updated.known, old.known);
    assert_eq!(updated.protected, old.protected);
    assert_eq!(updated.atlas, old.atlas);
    assert_eq!(updated.tint, old.tint);
    let net::height::Replacement::Column { geometry, .. } = &updated.replacement else {
        unreachable!()
    };
    let stamp = geometry.iter().find(|s| s.page == page(2)).unwrap();
    assert_eq!(
        (stamp.source, stamp.view),
        (
            h.geometry[&page(2)].source_version,
            h.geometry[&page(2)].view_version
        )
    );
    let changed = h.water(&updated, 2);
    assert_eq!(changed.column_source, updated.source);
    assert!(changed.water_view > water.water_view);
    assert_eq!(changed.cells[halo_index(pos(31, 40, 8), page(2))], 1);
}

fn recert_projection_trace(messages: &[ServerMessage]) -> Vec<Vec<u8>> {
    messages
        .iter()
        .filter(|m| {
            matches!(
                m,
                ServerMessage::Geometry(_)
                    | ServerMessage::GeometryUnavailable(_)
                    | ServerMessage::Height(_)
                    | ServerMessage::HeightUnavailable(_)
                    | ServerMessage::WaterHalo(_)
                    | ServerMessage::WaterUnavailable(_)
            )
        })
        .map(|m| net::encode_server(m).unwrap())
        .collect()
}
#[test]
fn private_recertification_is_wire_silent_explicit_full_survives_and_public_withdrawal_is_immediate()
 {
    let (world, _, _) = worlds();
    let directory = Directory::new();
    let mut h = H::open(&directory, world);
    let height = h.exact();
    let water = h.water(&height, 2);
    h.send(ClientMessage::RequestWater(Request {
        page: page(2),
        geometry_source: water.geometry_source,
        geometry_view: water.geometry_view,
        column_source: water.column_source,
        acknowledged: Some(water.water_view),
    }));
    let first_geometry = net::encode_server(&ServerMessage::Geometry(Box::new(
        h.geometry[&page(2)].clone(),
    )))
    .unwrap();
    let first_height = net::encode_server(&ServerMessage::Height(height.clone())).unwrap();
    for _ in 0..50 {
        h.step()
    }
    h.out.clear();
    let hidden = pos(21, 35, 3);
    assert_eq!(h.server.world().read(hidden), CellRead::Known(Cell::AIR));
    let old_page = h.server.world().page_version(hidden.page());
    let submitted = h.server.counters().geometry_submitted;
    let mut tx = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    tx.set_cell(hidden, Cell::from_parts(rules::ids::LAVA, 0));
    assert!(h.server.queue_transaction(tx).is_ok());
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        h.step();
        let stats = h.server.cold_projection_stats().unwrap();
        if h.server.world().page_version(hidden.page()) > old_page
            && !h.server.awaiting_durability()
            && h.server.counters().geometry_submitted > submitted
            && stats.queued == 0
            && stats.running == 0
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real private durable edit must recertify"
        );
    }
    for _ in 0..50 {
        h.step()
    }
    assert!(
        recert_projection_trace(&h.out).is_empty(),
        "private-only recertification emitted public Geometry/Height/Water headers/bodies/Unavailable"
    );
    assert_eq!(
        h.server.world().read(hidden),
        CellRead::Known(Cell::from_parts(rules::ids::LAVA, 0))
    );
    // Explicit callers can still demand the same complete public image.
    h.send(ClientMessage::RequestGeometry {
        page: page(2),
        known: None,
    });
    let full = h.until(|m| matches!(m,ServerMessage::Geometry(g)if g.page==page(2)));
    assert_eq!(net::encode_server(&full).unwrap(), first_geometry);
    h.send(ClientMessage::RequestExactHeight {
        key: key(),
        known: None,
    });
    let full = h.until(|m| matches!(m,ServerMessage::Height(t)if t.key==key()));
    assert_eq!(net::encode_server(&full).unwrap(), first_height);
    // A visible sky voxel is a positive public-source control. Its previous
    // water binding is denied synchronously, rather than treated as private WAIT.
    let sky = pos(31, 127, 8);
    assert_eq!(h.server.world().read(sky), CellRead::Known(Cell::AIR));
    let mut tx = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    tx.set_cell(sky, Cell::from_parts(rules::ids::STONE, 0));
    assert!(h.server.queue_transaction(tx).is_ok());
    h.until(|m| matches!(m,ServerMessage::Height(t)if t.key==key()&&t.source!=height.source));
    h.out.clear();
    let inbox_free = h.server.inbox_capacity();
    h.send(ClientMessage::RequestWater(Request {
        page: page(2),
        geometry_source: water.geometry_source,
        geometry_view: water.geometry_view,
        column_source: water.column_source,
        acknowledged: Some(water.water_view),
    }));
    // handle() admits to the bounded inbox. A response belongs to the next
    // actual domain tick and its durable outbox, not to transport enqueue.
    let admission_deadline = Instant::now() + Duration::from_secs(40);
    while h.server.inbox_capacity() != inbox_free {
        assert!(
            Instant::now() < admission_deadline,
            "water request was not processed"
        );
        h.step();
    }
    if h.server.awaiting_durability() {
        let batches = h.server.counters().durable_batches;
        while h.server.counters().durable_batches == batches {
            assert!(
                Instant::now() < admission_deadline,
                "water response batch was not durable"
            );
            h.step();
        }
    }
    assert!(
        h.out
            .iter()
            .any(|m| matches!(m,ServerMessage::WaterUnavailable(p)if *p==page(2))),
        "a genuinely replaced public binding is withdrawn in the request response"
    );
}
