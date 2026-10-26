//! Missing graph input must retry a future tick, allowing real generation to run.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use net::{ClientMessage, ServerMessage};
use rules::{
    blocks::MekKind,
    ids::*,
    items::I_DUST_IRON,
    machines::{Machine, Slot},
    stack::ItemStack,
};
use server::{Server, ServerConfig};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
fn p(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 2,
        z: 8,
    }
}
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn step(server: &mut Server, now: &mut u64) {
    *now += 10;
    server.tick(*now);
    assert!(!server.is_failed(), "{:?}", server.failure_reason());
    thread::sleep(Duration::from_micros(500));
}
fn drain(server: &mut Server, now: u64) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        server.tick(now);
        let m = server.load_metrics();
        if !m.awaiting_durability && !m.machine_phase_pending && !m.mob_phase_pending {
            break;
        }
        assert!(Instant::now() < deadline, "held owner {:?}", m);
        thread::sleep(Duration::from_micros(500));
    }
}
#[test]
fn unknown_network_edge_retries_without_holding_tick_and_generation_unblocks_it() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/tmp")
        .join(format!("machine-owner-unknown-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let directory = Directory(root);
    let mut config = ServerConfig::new(&directory.0);
    config.generation_enabled = true;
    config.spawn_override = Some([8.5, 1., 8.5]);
    let mut world = World::new(rules::properties, 4096);
    let mut cells = vec![Cell::AIR; 4096];
    for cell in &mut cells[..256] {
        *cell = Cell::from_parts(STONE, 0);
    }
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(p(0).page(), cells);
    let mut source = Machine::new(MekKind::Cube);
    source.energy = 50_000;
    tx.set_cell(p(14), Cell::from_parts(ENERGY_CUBE, 5));
    tx.set_component(
        ComponentId(1),
        server::machines::machine_component(p(14), &source),
        ChannelMask::CONTENTS,
    );
    tx.set_cell(p(15), Cell::from_parts(UNIVERSAL_CABLE, 0));
    let mut smelter = Machine::new(MekKind::Smelter);
    smelter.energy = 10_000;
    smelter.slots[0] = Slot::new(ItemStack::new(I_DUST_IRON, 1).unwrap());
    tx.set_cell(p(6), Cell::from_parts(ENERGIZED_SMELTER, 2));
    tx.set_component(
        ComponentId(2),
        server::machines::machine_component(p(6), &smelter),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut server = Server::open_with_world(config, world).unwrap();
    let hello =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap();
    let mut now = 0;
    let mut out = server.handle(1, hello, now);
    let until = Instant::now() + Duration::from_secs(30);
    while !out
        .iter()
        .any(|o| matches!(o.message, ServerMessage::Welcome(_)))
    {
        now += 10;
        out.extend(server.tick(now));
        assert!(!server.is_failed());
        assert!(Instant::now() < until, "join {:?}", server.load_metrics());
        thread::sleep(Duration::from_micros(500));
    }
    let begin = server.current_tick().0;
    while server.current_tick().0 < begin + 8 {
        step(&mut server, &mut now);
        assert!(
            Instant::now() < until,
            "unknown input must not hold tick {:?}",
            server.load_metrics()
        );
    }
    drain(&mut server, now);
    assert!(
        server.counters().topology_retried > 0 || server.counters().machine_unknown > 0,
        "fixture must exercise an actual missing graph input"
    );
    assert!(
        server
            .world()
            .page(PageId {
                space: SpaceId(0),
                x: 1,
                y: 0,
                z: 0
            })
            .is_none()
    );
    let before = server::machines::read_machine(server.world(), ComponentId(2)).unwrap();
    let before_tick = server.current_tick();
    assert!(
        before.progress > 0,
        "unrelated known recipe runs while the graph input is Unknown"
    );
    server.handle(
        1,
        ClientMessage::RequestPage {
            page: PageId {
                space: SpaceId(0),
                x: 1,
                y: 0,
                z: 0,
            },
            acknowledged: None,
        },
        now,
    );
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        step(&mut server, &mut now);
        if server
            .world()
            .page(PageId {
                space: SpaceId(0),
                x: 1,
                y: 0,
                z: 0,
            })
            .is_some()
            && server.machine_runtime().network_at(p(15)).is_some()
            && server.current_tick().0 >= before_tick.0 + 8
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "real generation/topology progress {:?} {:?}",
            server.load_metrics(),
            server.failure_reason()
        );
    }
    drain(&mut server, now);
    let elapsed = server.current_tick().0 - before_tick.0;
    let after = server::machines::read_machine(server.world(), ComponentId(2)).unwrap();
    assert!(elapsed < 100, "bounded known generation fixture");
    assert_eq!(after.energy, before.energy - 50 * elapsed);
    assert_eq!(
        u64::from(after.progress),
        u64::from(before.progress) + elapsed
    );
    assert!(!server.load_metrics().machine_phase_pending);
}
