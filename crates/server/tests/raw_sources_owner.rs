//! LIGHT and unrelated page births cannot invalidate an unchanged raw image.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use net::{ClientMessage, InputFrame, PageUpdate, ServerMessage};
use server::{Outgoing, Server, ServerConfig};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct H {
    server: Server,
    now: u64,
    out: Vec<Outgoing>,
}
impl H {
    fn step(&mut self) {
        self.now += 10;
        self.out.extend(self.server.tick(self.now));
        assert!(!self.server.is_failed());
        thread::sleep(Duration::from_micros(300));
    }
    fn send(&mut self, message: ClientMessage) {
        self.out.extend(self.server.handle(1, message, self.now));
    }
    fn until(&mut self, mut f: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(i) = self.out.iter().position(&mut f) {
                return self.out.remove(i);
            }
            assert!(Instant::now() < deadline, "{:?}", self.server.counters());
            self.step();
        }
    }
    fn image(&mut self, page: PageId) -> net::PageSnapshot {
        self.send(ClientMessage::RequestPage {
            page,
            acknowledged: None,
        });
        let image = self.until(
            |o| matches!(&o.message,ServerMessage::Page(PageUpdate::Snapshot(s))if s.page==page),
        );
        let ServerMessage::Page(PageUpdate::Snapshot(s)) = image.message else {
            unreachable!()
        };
        s
    }
    fn drain(&mut self, steps: u64) {
        for _ in 0..steps {
            self.step();
        }
        while self.server.awaiting_durability() {
            self.out.extend(self.server.tick(self.now));
            thread::sleep(Duration::from_micros(300));
        }
    }
}
#[test]
fn durable_cell_source_ignores_light_and_contents_revisits_but_tracks_channel_none_metadata() {
    let directory = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("raw-source-owner-{}", std::process::id())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let page = PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    };
    let position = BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 0,
        z: 8,
    };
    let mut world = World::new(rules::properties, 4096);
    let mut cells = vec![Cell::AIR; 4096];
    cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(page, cells);
    world.commit(tx).unwrap();
    let mut config = ServerConfig::new(&directory.0);
    config.generation_enabled = false;
    config.spawn_override = Some([8.5, 1., 8.5]);
    let mut h = H {
        server: Server::open_with_world(config, world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let owner =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap();
    h.send(owner);
    h.until(|o| matches!(o.message, ServerMessage::Welcome(_)));
    let first = h.image(page);
    h.send(ClientMessage::AckPage {
        page,
        version: first.version,
    });
    h.drain(20);
    h.out.clear();
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    tx.set_light(
        BlockPos { y: 1, ..position },
        world::LightSample {
            sky: 9,
            block: 3,
            rgb: [17, 23, 255],
        },
    );
    assert!(h.server.queue_transaction(tx).is_ok());
    h.drain(20);
    assert!(h.server.world().page_version(page) > first.source_version);
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    tx.set_component(
        ComponentId(707),
        world::ComponentData {
            anchor: position,
            kind: 707,
            payload: b"private-raw-source-canary".to_vec(),
        },
        ChannelMask::CONTENTS,
    );
    assert!(h.server.queue_transaction(tx).is_ok());
    h.drain(20);
    // A birth elsewhere forces visibility checks of all near subscriptions.
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    tx.create_page(PageId { x: 100, ..page }, vec![Cell::AIR; 4096]);
    assert!(h.server.queue_transaction(tx).is_ok());
    h.drain(20);
    h.send(ClientMessage::Inputs(vec![InputFrame {
        sequence: 1,
        keys: InputFrame::FORWARD,
        ..InputFrame::default()
    }]));
    h.until(
        |o| matches!(&o.message,ServerMessage::Prediction{player,..}if player.input_sequence==1),
    );
    h.drain(20);
    assert!(
        !h.out
            .iter()
            .any(|o| matches!(&o.message,ServerMessage::Page(p)if match p{
             PageUpdate::Snapshot(p)=>p.page==page&&p.version>first.version,
             PageUpdate::Delta(p)=>p.page==page&&p.version>first.version,
            }))
    );
    let repeated = h.image(page);
    assert_eq!(repeated.version, first.version);
    assert_eq!(repeated.source_version, first.source_version);
    h.send(ClientMessage::AckPage {
        page,
        version: repeated.version,
    });
    h.out.clear();
    let cell = Cell::from_parts(rules::ids::STONE, 2);
    assert_eq!(
        rules::properties(cell)
            .changed_channels(rules::properties(Cell::from_parts(rules::ids::STONE, 0))),
        ChannelMask::NONE
    );
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    tx.set_cell(position, cell);
    assert!(h.server.queue_transaction(tx).is_ok());
    h.drain(20);
    let changed = h.image(page);
    assert!(changed.version > first.version);
    assert!(changed.source_version > first.source_version);
    assert_eq!(
        changed.cells[usize::from(position.local_index().get())],
        cell
    );
}
