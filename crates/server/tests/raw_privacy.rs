//! Encoded near Raw/LIGHT privacy, confirmed-view bindings and observer access.
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use net::{
    Action, ActionId, ActionStatus, ClientMessage, PageUpdate, Replica, ReplicaRead, ServerMessage,
};
use server::{Server, ServerConfig};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{CellRead, Transaction, World};
#[path = "support/private_pocket.rs"]
mod pocket;
use pocket::{page, pos, worlds};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "raw-private-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
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
    replica: Replica,
    next_action: u64,
    raw_packets: u64,
    max_rays_per_call: u64,
}
impl H {
    fn open(d: &Directory, world: World, spawn: [f64; 3]) -> Self {
        let (store, _) = save::Store::open(&d.0).unwrap();
        store
            .load_or_create_private_entropy(|| Ok([37; 32]))
            .unwrap();
        drop(store);
        let mut c = ServerConfig::new(&d.0);
        c.public_seed = 1;
        c.generation_enabled = false;
        c.spawn_protection_radius = 0;
        c.spawn_override = Some(spawn);
        let mut h = Self {
            server: Server::open_with_world(c, world).unwrap(),
            now: 0,
            out: Vec::new(),
            replica: Replica::new(16),
            next_action: 1,
            raw_packets: 0,
            max_rays_per_call: 0,
        };
        let hello =
            net::decode_client(&std::fs::read(d.0.join("owner.identity")).unwrap()).unwrap();
        h.send(hello);
        h.until(|m| matches!(m, ServerMessage::Welcome(_)));
        h.action(Action::Command("/gamemode creative".into()));
        h.action(Action::Flight(true));
        let end = Instant::now() + Duration::from_secs(60);
        while !h.server.height_ready_for_publication() || h.server.awaiting_durability() {
            assert!(Instant::now() < end);
            h.step()
        }
        h
    }
    fn collect(&mut self, output: Vec<server::Outgoing>) {
        for o in output {
            let bytes = net::encode_server(&o.message).unwrap();
            let m = net::decode_server(&bytes).unwrap();
            if let ServerMessage::Page(update) = &m {
                self.raw_packets += 1;
                self.replica.apply(update.clone()).unwrap();
            }
            self.out.push(m)
        }
    }
    fn send(&mut self, m: ClientMessage) {
        let bytes = net::encode_client(&m).unwrap();
        let before = self.server.counters();
        let output = self
            .server
            .handle(1, net::decode_client(&bytes).unwrap(), self.now);
        self.collect(output);
        self.note_budget(before.visibility_rays)
    }
    fn note_budget(&mut self, before: u64) {
        let rays = self.server.counters().visibility_rays - before;
        self.max_rays_per_call = self.max_rays_per_call.max(rays);
        assert!(
            rays <= 128,
            "one call can finish the previous durability boundary and start at most one next logical tick: {rays}"
        );
    }
    fn step(&mut self) {
        self.now += 10;
        let before = self.server.counters();
        let output = self.server.tick(self.now);
        self.collect(output);
        self.note_budget(before.visibility_rays);
        assert!(
            !self.server.is_failed(),
            "{:?}",
            self.server.failure_reason()
        );
        thread::sleep(Duration::from_micros(300))
    }
    fn until(&mut self, mut f: impl FnMut(&ServerMessage) -> bool) -> ServerMessage {
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(i) = self.out.iter().position(&mut f) {
                return self.out.remove(i);
            }
            assert!(
                Instant::now() < end,
                "privacy timeout counters{:?}",
                self.server.counters()
            );
            self.step()
        }
    }
    fn action(&mut self, action: Action) {
        let id = ActionId(self.next_action);
        self.next_action += 1;
        self.send(ClientMessage::Action { id, action });
        let ServerMessage::Action(reply) = self.until(
            |m| matches!(m,ServerMessage::Action(r)if r.id==id&&r.status!=ActionStatus::Pending),
        ) else {
            unreachable!()
        };
        assert_eq!(reply.status, ActionStatus::Accepted)
    }
    fn raw(&mut self, p: PageId, known: Option<u64>) -> PageUpdate {
        self.out
            .retain(|m| !matches!(m,ServerMessage::Page(update)if update_page(update)==p));
        self.send(ClientMessage::RequestPage {
            page: p,
            acknowledged: known,
        });
        let ServerMessage::Page(update) =
            self.until(|m| matches!(m,ServerMessage::Page(update)if update_page(update)==p))
        else {
            unreachable!()
        };
        update
    }
    fn confirm(&mut self, p: PageId) -> u64 {
        let version = self.replica.page(p).unwrap().version;
        self.send(ClientMessage::AckPage { page: p, version });
        version
    }
    fn light(&mut self, p: PageId, view: u64, ack: Option<u64>) -> net::light::Update {
        self.send(ClientMessage::RequestLight(net::light::Request {
            page: p,
            cell_view: view,
            acknowledged: ack,
        }));
        let ServerMessage::Light(light)=self.until(|m|matches!(m,ServerMessage::Light(light)if light.header().page==p&&light.header().cell_view==view))else{unreachable!()};
        *light
    }
    fn settled_light(&mut self) {
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            let c = self.server.counters();
            if !self.server.awaiting_durability()
                && c.light_pending_cells == 0
                && c.light_pending_seed_pages == 0
                && c.light_pending_bootstrap_pages == 0
                && c.light_pending_sky_pages == 0
            {
                return;
            }
            assert!(Instant::now() < end, "lighting did not settle");
            self.step()
        }
    }
    fn commit(&mut self, tx: Transaction) {
        let old = self.server.world().version();
        assert!(self.server.queue_transaction(tx).is_ok());
        let end = Instant::now() + Duration::from_secs(60);
        while self.server.world().version() == old || self.server.awaiting_durability() {
            assert!(Instant::now() < end);
            self.step()
        }
    }
}
fn update_page(p: &PageUpdate) -> PageId {
    match p {
        PageUpdate::Snapshot(s) => s.page,
        PageUpdate::Delta(d) => d.page,
    }
}
fn normalized_raw(mut p: net::PageSnapshot) -> Vec<u8> {
    p.version = 1;
    net::encode_server(&ServerMessage::Page(PageUpdate::Snapshot(p))).unwrap()
}
fn normalized_light(mut p: net::light::Update) -> Vec<u8> {
    match &mut p {
        net::light::Update::Snapshot(s) => {
            s.header.version = 1;
            s.header.cell_view = 1
        }
        net::light::Update::Delta(_) => panic!("initial admitted light requires full image"),
    };
    net::encode_server(&ServerMessage::Light(Box::new(p))).unwrap()
}
fn sample_light(h: &H, p: BlockPos) -> world::LightSample {
    h.server
        .world()
        .page(p.page())
        .unwrap()
        .light(p.local_index())
}

#[test]
fn hidden_raw_edit_cannot_change_encoded_public_source_after_forget_and_reentry() {
    let (actual, baseline, component) = worlds();
    let da = Directory::new();
    let db = Directory::new();
    let hidden = *component
        .iter()
        .find(|p| p.page() == page(2) && actual.read(**p) == CellRead::Known(Cell::AIR))
        .unwrap();
    let mut a = H::open(&da, actual, [24.5, 80., 8.5]);
    let mut b = H::open(&db, baseline, [24.5, 80., 8.5]);
    a.raw(page(2), None);
    b.raw(page(2), None);
    assert_eq!(
        normalized_raw(a.replica.page(page(2)).unwrap().clone()),
        normalized_raw(b.replica.page(page(2)).unwrap().clone())
    );
    let mut transaction = Transaction::new(a.server.current_tick(), SpaceId(0));
    transaction.set_cell(hidden, Cell::from_parts(rules::ids::LAVA, 0));
    a.commit(transaction);
    // The client chooses the same public lease lifecycle in both worlds. Only
    // one hidden authoritative edit differs; no own action ID was consumed.
    for h in [&mut a, &mut b] {
        h.send(ClientMessage::ForgetPage(page(2)));
        h.replica.remove(page(2));
        h.until(|m| matches!(m,ServerMessage::PageUnavailable(p) if *p==page(2)));
        h.raw(page(2), None);
    }
    let a = a.replica.page(page(2)).unwrap().clone();
    let b = b.replica.page(page(2)).unwrap().clone();
    assert_eq!(
        a.cells, b.cells,
        "the closed component still has the same public body"
    );
    assert_eq!(
        normalized_raw(a),
        normalized_raw(b),
        "history eviction must not expose the last private page mutation stamp"
    );
}

#[test]
fn first_raw_lease_does_not_serialize_a_preexisting_hidden_page_epoch() {
    let (mut actual, baseline, component) = worlds();
    let hidden = *component
        .iter()
        .find(|p| p.page() == page(2) && actual.read(**p) == CellRead::Known(Cell::AIR))
        .unwrap();
    let mut transaction = Transaction::new(Tick(1), SpaceId(0));
    transaction.set_cell(hidden, Cell::from_parts(rules::ids::LAVA, 0));
    actual.commit(transaction).unwrap();
    let da = Directory::new();
    let db = Directory::new();
    let mut a = H::open(&da, actual, [24.5, 80., 8.5]);
    let mut b = H::open(&db, baseline, [24.5, 80., 8.5]);
    a.raw(page(2), None);
    b.raw(page(2), None);
    assert_eq!(
        normalized_raw(a.replica.page(page(2)).unwrap().clone()),
        normalized_raw(b.replica.page(page(2)).unwrap().clone()),
        "the first lease must be independent of the private mutation history too"
    );
}
#[test]
fn closed_private_component_encoded_raw_and_light_equal_filled_baseline_with_no_partial_output() {
    let (aw, bw, component) = worlds();
    let da = Directory::new();
    let db = Directory::new();
    let mut a = H::open(&da, aw, [24.5, 80., 8.5]);
    let mut b = H::open(&db, bw, [24.5, 80., 8.5]);
    a.settled_light();
    b.settled_light();
    let lava = *component
        .iter()
        .find(|&&p| {
            a.server.world().read(p) == CellRead::Known(Cell::from_parts(rules::ids::LAVA, 0))
        })
        .unwrap();
    assert!(
        sample_light(&a, lava).block > 0,
        "actual canonical lava must be lit before testing masking"
    );
    assert!(
        component
            .iter()
            .filter(|p| p.page() == page(2))
            .any(|p| sample_light(&a, *p).block > 0),
        "the requested page must contain actual hidden radiance"
    );
    assert_eq!(a.raw_packets, 0);
    let before = a.server.counters().visibility_rays;
    let _ = a.raw(page(2), None);
    let used = a.server.counters().visibility_rays - before;
    let candidates = component.iter().filter(|p| p.page() == page(2)).count();
    assert!(candidates > 64);
    assert!(
        used >= candidates as u64,
        "complete image must certify every hidden component cell"
    );
    let _ = b.raw(page(2), None);
    for p in component.iter().filter(|p| p.page() == page(2)) {
        assert_eq!(
            a.replica.read(*p),
            ReplicaRead::Known(Cell::from_parts(rules::ids::STONE, 0))
        );
    }
    assert_eq!(
        normalized_raw(a.replica.page(page(2)).unwrap().clone()),
        normalized_raw(b.replica.page(page(2)).unwrap().clone()),
        "private raw source must not be hidden LIGHT transaction version"
    );
    let unconfirmed = a.replica.page(page(2)).unwrap().version;
    a.send(ClientMessage::RequestLight(net::light::Request {
        page: page(2),
        // A valid RequestLight also acknowledges its retained raw view. A
        // forged not-yet-published view must not grant that confirmation.
        cell_view: unconfirmed + 1,
        acknowledged: None,
    }));
    let until = a.server.current_tick().0 + 2;
    while a.server.current_tick().0 < until || a.server.awaiting_durability() {
        a.step();
    }
    assert!(
        !a.out
            .iter()
            .any(|m| matches!(m,ServerMessage::Light(light)if light.header().page==page(2))),
        "an unpublished raw view cannot authorize a LIGHT image"
    );
    let av = a.confirm(page(2));
    let bv = b.confirm(page(2));
    let al = a.light(page(2), av, None);
    let bl = b.light(page(2), bv, None);
    assert_eq!(
        normalized_light(al.clone()),
        normalized_light(bl),
        "including public LIGHT source, only stream view numbers normalize"
    );
    let net::light::Update::Snapshot(light) = al else {
        unreachable!()
    };
    for p in component.iter().filter(|p| p.page() == page(2)) {
        assert_eq!(light.values[usize::from(p.local_index().get())], 0);
    }
    let hidden = pos(21, 35, 3);
    let private_before = a
        .server
        .world()
        .page(page(2))
        .unwrap()
        .channel_version(core_types::ChannelMask::LIGHTING);
    a.send(ClientMessage::RequestLight(net::light::Request {
        page: page(2),
        cell_view: av,
        acknowledged: Some(light.header.version),
    }));
    a.out.retain(|m| !matches!(m, ServerMessage::Light(_)));
    let mut tx = Transaction::new(Tick(a.server.current_tick().0 + 1), SpaceId(0));
    tx.set_light(
        hidden,
        world::LightSample {
            sky: 0,
            block: 15,
            rgb: [17, 29, 43],
        },
    );
    a.commit(tx);
    assert!(
        a.server
            .world()
            .page(page(2))
            .unwrap()
            .channel_version(core_types::ChannelMask::LIGHTING)
            > private_before
    );
    assert!(
        !a.out
            .iter()
            .any(|m| matches!(m,ServerMessage::Light(update)if update.header().page==page(2))),
        "hidden-only light input cannot create a public delta or advance its metadata"
    );
    let repair = a.light(page(2), av, None);
    assert_eq!(repair.header().source, light.header.source);
    assert_eq!(repair.header().version, light.header.version);
    let net::light::Update::Snapshot(repair) = repair else {
        unreachable!()
    };
    assert_eq!(repair.values, light.values);
    assert!(a.max_rays_per_call <= 128);
    assert_eq!(
        a.server.world().read(pos(21, 35, 3)),
        CellRead::Known(Cell::AIR)
    );
}
#[test]
fn hidden_air_lava_cell_changes_keep_raw_and_light_wire_headers_and_bodies() {
    let (world, _, _) = worlds();
    let d = Directory::new();
    let mut h = H::open(&d, world, [24.5, 80., 8.5]);
    h.settled_light();
    let hidden = pos(21, 35, 3);
    let p = hidden.page();
    assert_eq!(h.server.world().read(hidden), CellRead::Known(Cell::AIR));
    let _ = h.raw(p, None);
    let view = h.confirm(p);
    let raw = h.replica.page(p).unwrap().clone();
    let light = h.light(p, view, None);
    let light_bytes = net::encode_server(&ServerMessage::Light(Box::new(light.clone()))).unwrap();
    h.send(ClientMessage::RequestLight(net::light::Request {
        page: p,
        cell_view: view,
        acknowledged: Some(light.header().version),
    }));
    for replacement in [Cell::from_parts(rules::ids::LAVA, 0), Cell::AIR] {
        h.out.clear();
        let source = h.server.world().page_version(p);
        let mut tx = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
        tx.set_cell(hidden, replacement);
        h.commit(tx);
        assert!(h.server.world().page_version(p) > source);
        assert_eq!(h.server.world().read(hidden), CellRead::Known(replacement));
        // This requires a fresh private certificate. A retained public image
        // alone cannot satisfy the LIGHT guard after its private input changed.
        h.send(ClientMessage::RequestPage {
            page: p,
            acknowledged: Some(view),
        });
        h.settled_light();
        let repaired = h.light(p, view, None);
        assert_eq!(
            net::encode_server(&ServerMessage::Light(Box::new(repaired))).unwrap(),
            light_bytes,
            "hidden cell and emission changes must retain every public LIGHT header and value"
        );
        for update in h.out.iter().filter_map(|m| match m {
            ServerMessage::Page(update) if update_page(update) == p => Some(update),
            _ => None,
        }) {
            match update {
                // A requested full repair may repeat the exact retained image.
                // It must not reveal the private certificate in any header.
                PageUpdate::Snapshot(snapshot) => assert_eq!(snapshot, &raw),
                PageUpdate::Delta(delta) => panic!(
                    "a hidden-only cell update emitted Raw delta {} -> {} with {} cells",
                    delta.base,
                    delta.version,
                    delta.changes.len()
                ),
            }
        }
        assert_eq!(h.replica.page(p).unwrap(), &raw);
        h.send(ClientMessage::RequestLight(net::light::Request {
            page: p,
            cell_view: view,
            acknowledged: Some(light.header().version),
        }));
    }
}
#[test]
fn observer_inside_closed_cave_receives_real_body_air_and_light_rebases_without_input_change() {
    let (world, _, component) = worlds();
    let inside = *component
        .iter()
        .filter(|&&p| {
            world.read(p) == CellRead::Known(Cell::AIR)
                && world.read(pos(p.x, p.y + 1, p.z)) == CellRead::Known(Cell::AIR)
        })
        .min_by_key(|p| (p.page(), p.local_index().get()))
        .expect("two-cell canonical air body");
    let d = Directory::new();
    let mut h = H::open(&d, world, [24.5, 80., 8.5]);
    h.settled_light();
    let _ = h.raw(inside.page(), None);
    let outside = h.replica.page(inside.page()).unwrap().clone();
    assert_eq!(
        outside.cells[usize::from(inside.local_index().get())].id(),
        rules::ids::STONE
    );
    let old_view = h.confirm(inside.page());
    let first = h.light(inside.page(), old_view, None);
    let old_light = first.header();
    let input_source = h
        .server
        .world()
        .page(inside.page())
        .unwrap()
        .channel_version(core_types::ChannelMask::LIGHTING);
    // Start a real bounded repair while still outside and ensure the body cell
    // is already denied before teleport. The same raw/light inputs then remain
    // unchanged, exposing any pending cursor that mixes old and new observers.
    let actual = h.server.world().page(inside.page()).unwrap().cells();
    let body_index = usize::from(inside.local_index().get());
    let prefix = actual[..=body_index]
        .iter()
        .zip(&outside.cells[..=body_index])
        .filter(|(a, b)| a != b)
        .count();
    let total = actual
        .iter()
        .zip(outside.cells.iter())
        .filter(|(a, b)| a != b)
        .count();
    let required = prefix.div_ceil(64) * 64;
    assert!(
        required < total,
        "fixture must retain a pending tail after certifying body: prefix{prefix} total{total}"
    );
    let rays = h.server.counters().visibility_rays;
    let packets = h.raw_packets;
    h.send(ClientMessage::RequestPage {
        page: inside.page(),
        acknowledged: None,
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    while h.server.counters().visibility_rays - rays < required as u64 {
        assert!(Instant::now() < deadline);
        h.step();
        assert_eq!(h.raw_packets, packets, "partial view cannot be published");
    }
    h.action(Action::Command(format!(
        "/tp {} {} {}",
        f64::from(inside.x) + 0.5,
        inside.y,
        f64::from(inside.z) + 0.5
    )));
    let _ = h.raw(inside.page(), Some(old_view));
    let admitted = h.replica.page(inside.page()).unwrap().clone();
    assert!(admitted.version > old_view);
    let last_pose = h.out.iter().rev().find_map(|m| match m {
        ServerMessage::Prediction { player, .. } => Some(*player),
        _ => None,
    });
    eprintln!(
        "inside fixture {inside:?} latest player {last_pose:?}, oldView{old_view}, admittedView{}",
        admitted.version
    );
    for y in inside.y..inside.y + 2 {
        let p = pos(inside.x, y, inside.z);
        assert_eq!(
            h.replica.read(p),
            ReplicaRead::Known(Cell::AIR),
            "inside body point {p:?}, pose{last_pose:?}"
        );
        assert!(
            !rules::properties(admitted.cells[usize::from(p.local_index().get())]).collidable,
            "player's real air body cannot become false stone"
        );
    }
    assert_eq!(
        h.server
            .world()
            .page(inside.page())
            .unwrap()
            .channel_version(core_types::ChannelMask::LIGHTING),
        input_source
    );
    // An old acknowledged light image cannot be relabelled for a new raw view.
    let view = h.confirm(inside.page());
    let rebased = h.light(inside.page(), view, Some(old_light.version));
    assert!(matches!(rebased, net::light::Update::Snapshot(_)));
    assert_eq!(rebased.header().cell_view, view);
    assert!(rebased.header().version > old_light.version);
}
fn corridor() -> World {
    let mut w = World::new(rules::properties, 64);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in 0..3 {
        let mut cells = vec![Cell::from_parts(rules::ids::STONE, 0); 4096];
        for y in 1..3 {
            for z in 7..10 {
                for local_x in 0..16 {
                    cells[local_x + 16 * z + 256 * y] = Cell::AIR;
                }
            }
        }
        if x == 2 {
            cells[8 + 16 * 8 + 256 * 2] = Cell::from_parts(rules::ids::IRON_ORE, 0);
        }
        tx.create_page(
            PageId {
                space: SpaceId(0),
                x,
                y: 0,
                z: 0,
            },
            cells,
        );
    }
    w.commit(tx).unwrap();
    w
}
#[test]
fn real_wall_and_unknown_intermediate_page_close_observer_los_to_unchanged_target() {
    let d = Directory::new();
    let mut h = H::open(&d, corridor(), [8.5, 1., 8.5]);
    let target = pos(40, 2, 8);
    let p = target.page();
    let _ = h.raw(p, None);
    assert_eq!(
        h.replica.read(target),
        ReplicaRead::Known(Cell::from_parts(rules::ids::IRON_ORE, 0))
    );
    let first = h.confirm(p);
    let light = h.light(p, first, None);
    let source = h
        .server
        .world()
        .page(p)
        .unwrap()
        .channel_version(core_types::ChannelMask::LIGHTING);
    let mut wall = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    for y in 1..3 {
        for z in 7..10 {
            wall.set_cell(pos(24, y, z), Cell::from_parts(rules::ids::STONE, 0));
        }
    }
    h.commit(wall);
    let _ = h.raw(p, Some(first));
    assert_eq!(
        h.replica.read(target),
        ReplicaRead::Known(Cell::from_parts(rules::ids::STONE, 0))
    );
    let hidden = h.confirm(p);
    let _ = h.light(p, hidden, Some(light.header().version));
    let mut open = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    for y in 1..3 {
        for z in 7..10 {
            open.set_cell(pos(24, y, z), Cell::AIR);
        }
    }
    h.commit(open);
    let _ = h.raw(p, Some(hidden));
    assert_eq!(
        h.replica.read(target),
        ReplicaRead::Known(Cell::from_parts(rules::ids::IRON_ORE, 0))
    );
    let visible = h.confirm(p);
    let mut unknown = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    unknown.remove_page(PageId {
        space: SpaceId(0),
        x: 1,
        y: 0,
        z: 0,
    });
    h.commit(unknown);
    let _ = h.raw(p, Some(visible));
    assert_eq!(
        h.replica.read(target),
        ReplicaRead::Known(Cell::from_parts(rules::ids::STONE, 0)),
        "Unknown must terminate the actual observer ray"
    );
    assert_eq!(
        h.server
            .world()
            .page(p)
            .unwrap()
            .channel_version(core_types::ChannelMask::LIGHTING),
        source
    );
}
