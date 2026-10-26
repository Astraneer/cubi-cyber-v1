//! Indexed public hints follow confirmed raw cells and the durable owner frontier.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, SpaceId, Tick};
use net::{Action, ActionId, ActionStatus, ClientMessage, ServerMessage};
use rules::{stack::ItemStack, survival::furnace::Chest};
use server::{Outgoing, Server, ServerConfig};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
fn anchor() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 2,
        z: 11,
    }
}
const CHEST: ComponentId = ComponentId(100);
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
    fn until(&mut self, mut p: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(i) = self.out.iter().position(&mut p) {
                return self.out.remove(i);
            }
            assert!(
                Instant::now() < deadline,
                "public prop output {:?}",
                self.server.counters()
            );
            self.step();
        }
    }
    fn send(&mut self, m: ClientMessage) {
        self.out.extend(self.server.handle(1, m, self.now));
    }
    fn action(&mut self, id: u64, action: Action) {
        self.send(ClientMessage::Action {
            id: ActionId(id),
            action,
        });
        let output=self.until(|o|matches!(o.message,ServerMessage::Action(r)if r.id==ActionId(id)&&r.status!=ActionStatus::Pending));
        assert!(
            matches!(output.message,ServerMessage::Action(r)if r.status==ActionStatus::Accepted)
        );
    }
}
#[test]
fn confirmed_chest_lid_animates_but_private_contents_changes_send_no_visual_revision() {
    let directory = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("props-owner-{}", std::process::id())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let mut config = ServerConfig::new(&directory.0);
    config.generation_enabled = false;
    config.spawn_override = Some([8.5, 1., 8.5]);
    let mut world = World::new(rules::properties, 4096);
    let mut cells = vec![Cell::AIR; 4096];
    cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
    cells[usize::from(anchor().local_index().get())] = Cell::from_parts(rules::ids::CHEST, 0);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(anchor().page(), cells);
    tx.set_component(
        CHEST,
        server::survival::chest_component(anchor(), &Chest::default()),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut h = H {
        server: Server::open_with_world(config, world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let owner =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap();
    h.send(owner);
    h.until(|o| matches!(o.message, ServerMessage::Welcome(_)));
    h.send(ClientMessage::RequestPage {
        page: anchor().page(),
        acknowledged: None,
    });
    let page = h.until(|o| matches!(&o.message, ServerMessage::Page(_)));
    let ServerMessage::Page(net::PageUpdate::Snapshot(page)) = page.message else {
        panic!("initial snapshot")
    };
    for _ in 0..10 {
        h.step();
    }
    assert!(
        !h.out
            .iter()
            .any(|o| matches!(o.message, ServerMessage::Props(_))),
        "no hints before confirmed public cell base"
    );
    h.send(ClientMessage::AckPage {
        page: page.page,
        version: page.version,
    });
    let initial=h.until(|o|matches!(&o.message,ServerMessage::Props(f)if f.hints.iter().any(|hint|hint.kind==net::props::Kind::Chest)));
    let ServerMessage::Props(initial) = initial.message else {
        unreachable!()
    };
    assert_eq!(initial.cell_view, page.version);
    assert_eq!(initial.hints.len(), 1);
    assert_eq!(initial.hints[0].lid, 0);
    let before = h.server.counters().props_frames;
    let mut chest = Chest::default();
    chest
        .set_trusted(0, ItemStack::new(rules::items::I_DIAMOND, 37).unwrap())
        .unwrap();
    let old = h.server.world().component(CHEST).unwrap().version;
    let mut tx = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    tx.expect_page(
        anchor().page(),
        h.server.world().page_version(anchor().page()),
    );
    tx.set_component(
        CHEST,
        server::survival::chest_component(anchor(), &chest),
        ChannelMask::CONTENTS,
    );
    h.server
        .queue_transaction(tx)
        .unwrap_or_else(|_| panic!("owner queue"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.server.world().component(CHEST).unwrap().version == old {
        assert!(Instant::now() < deadline);
        h.step();
    }
    let target = h.server.current_tick().0 + 15;
    while h.server.current_tick().0 < target {
        h.step();
    }
    assert_eq!(
        h.server.counters().props_frames,
        before,
        "37 private diamonds never alter the public frame"
    );
    h.out.clear();
    h.action(
        1,
        Action::Window(net::window::WindowIntent::OpenBlock {
            position: anchor(),
            expected: Cell::from_parts(rules::ids::CHEST, 0),
        }),
    );
    let window = h.until(|o| matches!(&o.message,ServerMessage::Window(w)if !w.closed));
    let ServerMessage::Window(window) = window.message else {
        unreachable!()
    };
    let open = h.until(
        |o| matches!(&o.message,ServerMessage::Props(f)if f.hints.iter().any(|hint|hint.lid>0)),
    );
    let ServerMessage::Props(open) = open.message else {
        unreachable!()
    };
    assert!(open.view > initial.view);
    assert!(
        open.hints[0].lid < 65535,
        "actual bounded opening animation"
    );
    h.action(
        2,
        Action::Window(net::window::WindowIntent::Close {
            guard: window.guard,
        }),
    );
    h.until(|o| matches!(&o.message,ServerMessage::Props(f)if f.view>open.view&&f.hints[0].lid==0));
    assert!(
        h.server.counters().props_cell_reads < 1000,
        "model source visits indexed anchors, not4096cells per frame"
    );
    let request = ClientMessage::RequestProps {
        page: page.page,
        cell_view: page.version,
        known: None,
    };
    let bytes = net::encode_client(&request).unwrap();
    assert_eq!(net::decode_client(&bytes), Ok(request));
}
