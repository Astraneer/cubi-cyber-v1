//! Canonical8-page server leaf: a representation request changes neither world
//! source nor privacy, and requires an actually emitted8-page geometry manifest.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use gen_authority::{PrivateSeed, ServerGenerator};
use net::{
    ClientMessage, Hello, PublicGeometry, ServerMessage,
    height::{HeightKey, HeightTile, Replacement},
};
use server::{Outgoing, Server, ServerConfig};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{ComponentData, Transaction, World};
const PRIVATE: [u8; 32] = [37; 32];
const CANARY: &[u8] = b"EXACT_HEIGHT_PRIVATE_COMPONENT_NEVER_PUBLISHED";
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
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "exact-height-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn config(&self) -> ServerConfig {
        let mut c = ServerConfig::new(&self.0);
        c.public_seed = 1337;
        c.generation_enabled = false;
        c.spawn_override = Some([
            f64::from(key().x * 16) + 8.5,
            100.,
            f64::from(key().z * 16) + 8.5,
        ]);
        c
    }
    fn owner(&self) -> Hello {
        let ClientMessage::Hello(h) =
            net::decode_client(&std::fs::read(self.0.join("owner.identity")).unwrap()).unwrap()
        else {
            panic!("owner identity")
        };
        h
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(directory: &Directory) -> World {
    let (store, _) = save::Store::open(&directory.0).unwrap();
    store
        .load_or_create_private_entropy(|| Ok(PRIVATE))
        .unwrap();
    drop(store);
    let generator = ServerGenerator::new(r#gen::TerrainSeed(1337), PrivateSeed::new(PRIVATE));
    let column = generator.generate_column(key().x, key().z).unwrap();
    let mut world = World::new(rules::properties, 64);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for y in 0..8 {
        let cells: Box<[Cell]> = column.page(page(y), None).unwrap().cells;
        tx.create_page(page(y), cells);
    }
    tx.set_component(
        ComponentId(900),
        ComponentData {
            kind: 777,
            anchor: BlockPos {
                space: SpaceId(0),
                x: key().x * 16 + 8,
                y: 1,
                z: key().z * 16 + 8,
            },
            payload: CANARY.to_vec(),
        },
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    world
}
struct Harness {
    server: Server,
    now: u64,
    out: Vec<Outgoing>,
    geometry: BTreeMap<PageId, PublicGeometry>,
}
impl Harness {
    fn step(&mut self) {
        self.now += 10;
        let out = self.server.tick(self.now);
        self.collect(out);
        assert!(!self.server.is_failed());
        thread::sleep(Duration::from_micros(500));
    }
    fn collect(&mut self, out: Vec<Outgoing>) {
        for message in out {
            assert!(
                !matches!(message.message, ServerMessage::Page(_)),
                "exact height must not subscribe raw cells"
            );
            let bytes = net::encode_server(&message.message).unwrap();
            assert!(!bytes.windows(CANARY.len()).any(|w| w == CANARY));
            assert!(!bytes.windows(PRIVATE.len()).any(|w| w == PRIVATE));
            if let ServerMessage::Geometry(g) = &message.message {
                assert!(g.view_version > 0);
                assert_eq!(g.page.space, SpaceId(0));
                self.geometry.insert(g.page, (**g).clone());
            }
            self.out.push(message);
        }
    }
    fn send(&mut self, m: ClientMessage) {
        let wire = net::encode_client(&m).unwrap();
        let m = net::decode_client(&wire).unwrap();
        let out = self.server.handle(1, m, self.now);
        self.collect(out);
    }
    fn until(&mut self, mut predicate: impl FnMut(&ServerMessage) -> bool) -> ServerMessage {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(i) = self.out.iter().position(|o| predicate(&o.message)) {
                return self.out.remove(i).message;
            }
            assert!(
                Instant::now() < deadline,
                "height deadline counters={:?}",
                self.server.counters()
            );
            self.step();
        }
    }
    fn height(&mut self) -> HeightTile {
        let m = self.until(|m| {
            matches!(m,ServerMessage::Height(t)if t.key==key())
                || matches!(m,ServerMessage::HeightUnavailable(k)if *k==key())
        });
        let ServerMessage::Height(tile) = m else {
            panic!("requested exact leaf unavailable")
        };
        let bytes = net::height::encode(&tile).unwrap();
        net::height::decode(&bytes).unwrap()
    }
}
fn open(directory: &Directory) -> Harness {
    let mut h = Harness {
        server: Server::open_with_world(directory.config(), fixture(directory)).unwrap(),
        now: 0,
        out: vec![],
        geometry: BTreeMap::new(),
    };
    h.send(ClientMessage::Hello(directory.owner()));
    h.until(|m| matches!(m, ServerMessage::Welcome(_)));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !h.server.height_ready_for_publication() || h.server.awaiting_durability() {
        assert!(
            Instant::now() < deadline,
            "canonical private-baseline migration"
        );
        h.step();
    }
    h
}
fn column_manifest(h: &Harness, tile: &HeightTile) {
    let Replacement::Column {
        min_y,
        max_y,
        geometry,
    } = &tile.replacement
    else {
        panic!("missing exact column: {:?}", tile.replacement)
    };
    assert_eq!((*min_y, *max_y), (0, 7));
    assert_eq!(geometry.len(), 8);
    assert_eq!(tile.protected, [255; 32]);
    for (y, stamp) in geometry.iter().enumerate() {
        assert_eq!(stamp.page, page(y as i32));
        let actual = h
            .geometry
            .get(&stamp.page)
            .expect("manifest must reference actually published public geometry");
        assert_eq!(stamp.source, actual.source_version);
        assert_eq!(stamp.view, actual.view_version);
        assert_eq!(
            actual.source_version,
            h.server.world().input_version(
                stamp.page,
                ChannelMask::SHAPE | ChannelMask::OPACITY | ChannelMask::FLUID
            )
        );
        actual.validate().unwrap();
    }
}
#[test]
fn same_source_exact_request_promotes_canonical_height_to_real_eight_page_public_manifest() {
    let d = Directory::new();
    let mut h = open(&d);
    let world_before: Vec<_> = (0..8)
        .map(|y| h.server.world().page(page(y)).unwrap().cells())
        .collect();
    h.send(ClientMessage::RequestHeight {
        key: key(),
        known: None,
    });
    let normal = h.height();
    assert_eq!(normal.replacement, Replacement::None);
    assert!(normal.error.is_some());
    assert!(
        normal.error.unwrap().max_error_q1024 > 0,
        "non-flat leaf fixture: {:?}",
        normal.error
    );
    assert_eq!(normal.protected, [0; 32]);
    assert!(h.geometry.is_empty());
    let source = normal.source;
    let submitted = h.server.counters().geometry_published;
    h.send(ClientMessage::RequestExactHeight {
        key: key(),
        known: Some(source),
    });
    let exact = h.height();
    assert_eq!(
        exact.source, source,
        "representation changes do not invent a source revision"
    );
    assert_eq!(exact.heights, normal.heights);
    assert_eq!(exact.error, normal.error);
    column_manifest(&h, &exact);
    assert_eq!(h.server.counters().geometry_published - submitted, 8);
    assert_eq!(
        (0..8)
            .map(|y| h.server.world().page(page(y)).unwrap().cells())
            .collect::<Vec<_>>(),
        world_before,
        "exact representation is a read-only derived request"
    );
    // A later ordinary refresh must retain the chosen exact representation even
    // when its acknowledged source equals the original height-only source.
    h.send(ClientMessage::RequestHeight {
        key: key(),
        known: Some(source),
    });
    let sticky = h.height();
    assert_eq!(sticky.source, source);
    column_manifest(&h, &sticky);
    assert_eq!(
        sticky.replacement, exact.replacement,
        "cached exact geometry keeps its actual source/view"
    );
    assert_eq!(h.geometry.len(), 8);
    h.send(ClientMessage::ForgetHeight(key()));
    h.send(ClientMessage::RequestHeight {
        key: key(),
        known: None,
    });
    let released = h.height();
    assert_eq!(released.source, source);
    assert_eq!(released.replacement, Replacement::None);
    assert_eq!(
        (0..8)
            .map(|y| h.server.world().page(page(y)).unwrap().cells())
            .collect::<Vec<_>>(),
        world_before
    );
}
