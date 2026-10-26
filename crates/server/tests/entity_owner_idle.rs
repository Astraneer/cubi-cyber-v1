//! Actual owner observation: static public actors remain visible to a real
//! authenticated spectator. No runtime/body work is used to hide projection cost.
use core_types::{Cell, PageId, SpaceId, Tick};
use net::{Action, ActionId, ActionStatus, ClientMessage, ServerMessage};
use rules::entities::{EntityId, MobKind, mob::Mob};
use server::{Outgoing, Server, ServerConfig};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use telemetry::{AllocationCounts, AllocationScope};
use world::{Transaction, World};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator =
    telemetry::CountingAllocator::new(std::alloc::System);
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn config(directory: &Directory) -> ServerConfig {
    let mut c = ServerConfig::new(&directory.0);
    c.generation_enabled = false;
    c.spawn_override = Some([8.5, 1., 8.5]);
    c
}
fn collect(server: &mut Server, now: &mut u64, output: &mut Vec<Outgoing>) {
    *now += 10;
    output.extend(server.tick(*now));
    assert!(!server.is_failed(), "{:?}", server.failure_reason());
    thread::sleep(Duration::from_micros(100));
}
fn wait(
    server: &mut Server,
    now: &mut u64,
    output: &mut Vec<Outgoing>,
    predicate: impl Fn(&Outgoing) -> bool,
) -> Outgoing {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(index) = output.iter().position(&predicate) {
            return output.remove(index);
        }
        collect(server, now, output);
        assert!(Instant::now() < until, "real authority response");
    }
}
fn add(total: &mut AllocationCounts, value: AllocationCounts) {
    total.alloc_calls += value.alloc_calls;
    total.zeroed_alloc_calls += value.zeroed_alloc_calls;
    total.realloc_calls += value.realloc_calls;
    total.allocated_bytes += value.allocated_bytes;
    total.released_bytes += value.released_bytes;
}
#[test]
fn one_thousand_cold_owner_ticks_keep_public_actors_without_runtime_work() {
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let directory = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("entity-owner-idle-{suffix}")),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let mut world = World::new(rules::properties, 4096);
    let mut cells = vec![Cell::AIR; 4096];
    cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
    cells[4 * 256..5 * 256].fill(Cell::from_parts(rules::ids::STONE, 0));
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        cells,
    );
    world.commit(tx).unwrap();
    let mut server = Server::open_with_world(config(&directory), world).unwrap();
    let hello =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap();
    let mut now = 0;
    let mut out = server.handle(1, hello, now);
    wait(&mut server, &mut now, &mut out, |o| {
        matches!(o.message, ServerMessage::Welcome(_))
    });
    out.extend(server.handle(
        1,
        ClientMessage::Action {
            id: ActionId(1),
            action: Action::Command("/gamemode spectator".into()),
        },
        now,
    ));
    let reply = wait(
        &mut server,
        &mut now,
        &mut out,
        |o| matches!(o.message,ServerMessage::Action(r) if r.id==ActionId(1) && r.status!=ActionStatus::Pending),
    );
    assert!(matches!(reply.message,ServerMessage::Action(r) if r.status==ActionStatus::Accepted));
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        collect(&mut server, &mut now, &mut out);
        let c = server.counters();
        if !server.awaiting_durability()
            && server.height_ready_for_publication()
            && c.light_pending_cells == 0
            && c.light_pending_seed_pages == 0
            && c.light_pending_sky_pages == 0
            && c.light_pending_bootstrap_pages == 0
        {
            break;
        }
        assert!(Instant::now() < until);
    }
    assert!(server.request_checkpoint());
    drop(server); // The real writer joins after fsync of its accepted checkpoint.
    let (mut store, recovery) = save::Store::open(&directory.0).unwrap();
    let mut snapshot = recovery.snapshot.unwrap();
    assert!(recovery.batches.is_empty());
    let mut mobs = server::entities::MobRuntime::new(server::entities::Config::default()).unwrap();
    for i in 0..1000 {
        let mut mob = Mob::spawned(
            EntityId(10000 + i),
            MobKind::Pig,
            SpaceId(0),
            [3. + 0.2 * (i % 50) as f64, 1., 5. + 0.2 * (i / 50) as f64],
            i + 123,
        )
        .unwrap();
        mob.last_tick = snapshot.server_tick;
        mobs.insert_initial(mob, false).unwrap();
    }
    let record = snapshot
        .runtimes
        .iter_mut()
        .find(|r| r.kind == save::RUNTIME_MOBS)
        .unwrap();
    record.payload = mobs.snapshot().encode().unwrap();
    store.checkpoint(&snapshot).unwrap();
    drop(store);
    let mut server = Server::open(config(&directory)).unwrap();
    let hello =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap();
    out = server.handle(1, hello, now);
    wait(&mut server, &mut now, &mut out, |o| {
        matches!(o.message, ServerMessage::Welcome(_))
    });
    let frame = wait(&mut server, &mut now, &mut out, |o| {
        matches!(o.message, ServerMessage::Entities(_))
    });
    let ServerMessage::Entities(frame) = frame.message else {
        unreachable!()
    };
    assert_eq!(frame.upserts.len(), 1000);
    out.extend(server.handle(
        1,
        ClientMessage::Entities(net::entities::EntityRequest::Ack {
            revision: frame.revision,
        }),
        now,
    ));
    while server.awaiting_durability() {
        collect(&mut server, &mut now, &mut out);
    }
    out.clear();
    let before = server.counters();
    let begin = server.current_tick();
    let mut allocations = AllocationCounts::ZERO;
    for _ in 0..1000 {
        now += 50;
        let mut first = true;
        while first || server.awaiting_durability() {
            first = false;
            let scope = AllocationScope::begin().unwrap();
            let output = server.tick(now);
            add(&mut allocations, scope.finish());
            assert!(!server.is_failed(), "{:?}", server.failure_reason());
            assert!(
                output
                    .iter()
                    .all(|o| !matches!(o.message, ServerMessage::Entities(_))),
                "static confirmed public image must remain identical"
            );
            thread::sleep(Duration::from_micros(100));
        }
    }
    assert_eq!(server.current_tick().0, begin.0 + 1000);
    let after = server.counters();
    assert_eq!(server.mob_entities().len(), 1000);
    assert_eq!(server.mob_entities().active_len(), 0);
    assert_eq!(after.mob_steps, before.mob_steps);
    assert_eq!(after.runtime_handoff_bytes, before.runtime_handoff_bytes);
    assert!(
        after.entity_poses - before.entity_poses <= 1000,
        "unchanged confirmed sources permit at most one ACK warmup query"
    );
    println!(
        "cold_entity_owner ticks={} actors=1000 allocated_bytes={} allocation_calls={} public_pose_work={} public_bucket_work={} checkpoints={}",
        server.current_tick().0 - begin.0,
        allocations.allocated_bytes,
        allocations.allocating_calls(),
        after.entity_poses - before.entity_poses,
        after.entity_buckets - before.entity_buckets,
        after.checkpoint_requests - before.checkpoint_requests
    );
}
