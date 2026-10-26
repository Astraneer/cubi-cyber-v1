use super::{capture::Snapshot, owner::Owner};
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, Version};
use net::water::Request;
use std::{
    thread,
    time::{Duration, Instant},
};
use world::{LightSample, Transaction, World};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn p(x: i32, y: i32, z: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn b(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn request(page: PageId) -> Request {
    Request {
        page,
        geometry_source: Version(1),
        geometry_view: 1,
        column_source: 1,
        acknowledged: None,
    }
}
fn owner(peers: usize, pages: usize) -> Owner {
    Owner::new(
        r#gen::Generator::new(r#gen::TerrainSeed(1337)),
        peers,
        pages,
    )
    .unwrap()
}
fn fixture() -> World {
    let mut world = World::new(rules::properties, 256);
    for y in 1..=3 {
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        for z in -1..=1 {
            for x in -1..=1 {
                tx.create_page(p(x, y, z), vec![Cell::AIR; 4096]);
            }
        }
        world.commit(tx).unwrap();
    }
    world
}
fn await_ready(owner: &mut Owner, world: &World) -> super::owner::Ready {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(ready) = owner.poll(world) {
            return ready;
        }
        assert!(Instant::now() < end, "{:?}", owner.stats(Tick(0)));
        thread::sleep(Duration::from_micros(200));
    }
}
fn drain_job(owner: &mut Owner, world: &World) {
    let end = Instant::now() + Duration::from_secs(5);
    while owner.stats(Tick(0)).running > 0 {
        assert!(owner.poll(world).is_none());
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_micros(200));
    }
}
fn build(owner: &mut Owner, world: &World, now: Tick) -> Box<net::water::WaterHalo> {
    owner.begin_tick(now);
    let (key, request) = owner.next_request().expect("queued request");
    assert!(owner.submit(key, request, Snapshot::capture(world, key.1).unwrap(), now));
    let ready = await_ready(owner, world);
    owner.publish(ready, now).unwrap()
}
#[test]
fn fair_bounded_coalescing_and_per_tick_work_survive_one_busy_low_peer() {
    let mut owner = owner(4, 2);
    for connection in 1..=4 {
        for x in 0..2 {
            owner
                .request(connection, request(p(x, 2, 0)), Tick(0))
                .unwrap();
        }
    }
    for _ in 0..1000 {
        owner.request(1, request(p(0, 2, 0)), Tick(0)).unwrap();
    }
    assert!(owner.request(1, request(p(3, 2, 0)), Tick(0)).is_err());
    assert!(owner.request(5, request(p(0, 2, 0)), Tick(0)).is_err());
    owner.begin_tick(Tick(1));
    let ids: Vec<_> = (0..4).map(|_| owner.next_request().unwrap().0.0).collect();
    assert_eq!(ids, vec![1, 2, 3, 4]);
    assert!(owner.next_request().is_none());
    owner.begin_tick(Tick(2));
    let ids: Vec<_> = (0..4).map(|_| owner.next_request().unwrap().0.0).collect();
    assert_eq!(ids, vec![1, 2, 3, 4]);
    assert_eq!(owner.stats(Tick(2)).watcher_links, 8 * 27);
    owner.forget_peer(1);
    assert_eq!(owner.stats(Tick(2)).streams, 6);
    owner.retain_peers(|id| id == 3);
    assert_eq!(owner.stats(Tick(2)).watcher_links, 2 * 27);
}
#[test]
fn native_worker_rejects_stale_and_aba_results_and_keeps_one_unacknowledged_image() {
    let mut world = fixture();
    let mut owner = owner(2, 4);
    let req = request(p(0, 2, 0));
    owner.request(1, req, Tick(0)).unwrap();
    let first = build(&mut owner, &world, Tick(1));
    assert_eq!(first.cells.iter().filter(|&&v| v == 0).count(), 0);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_light(
        b(16, 33, 1),
        LightSample {
            sky: 15,
            block: 7,
            rgb: [7, 1, 2],
        },
    );
    let change = world.commit(tx).unwrap();
    owner.observe(&[change], Tick(1));
    assert_eq!(owner.stats(Tick(1)).queued, 0);
    for tick in 2..=1001 {
        let mut tx = Transaction::new(Tick(tick), SpaceId(0));
        tx.set_cell(
            b(16, 33, 1),
            Cell::from_parts(rules::ids::WATER, (tick % 7 + 1) as u16),
        );
        let change = world.commit(tx).unwrap();
        owner.observe(&[change], Tick(tick));
    }
    assert_eq!(
        owner.stats(Tick(1001)).queued,
        0,
        "unacknowledged image owns credit"
    );
    owner
        .request(
            1,
            Request {
                acknowledged: Some(first.water_view),
                ..req
            },
            Tick(1002),
        )
        .unwrap();
    assert_eq!(owner.stats(Tick(1002)).queued, 1);
    owner.begin_tick(Tick(1002));
    let (key, current) = owner.next_request().unwrap();
    assert!(owner.submit(
        key,
        current,
        Snapshot::capture(&world, key.1).unwrap(),
        Tick(1002)
    ));
    let mut tx = Transaction::new(Tick(1003), SpaceId(0));
    tx.set_cell(b(16, 33, 1), Cell::from_parts(rules::ids::STONE, 0));
    let change = world.commit(tx).unwrap();
    owner.observe(&[change], Tick(1003));
    drain_job(&mut owner, &world);
    assert_eq!(owner.stats(Tick(1003)).discarded, 1);
    let second = build(&mut owner, &world, Tick(1004));
    assert!(second.water_view > first.water_view);
    assert_eq!(second.geometry_view, first.geometry_view);
    assert_eq!(second.column_source, first.column_source);
    assert_eq!(second.cells[17 + 18 * (2 + 18 * 2)], 2);
    owner
        .request(
            1,
            Request {
                acknowledged: Some(second.water_view),
                ..req
            },
            Tick(1005),
        )
        .unwrap();
    let mut tx = Transaction::new(Tick(1005), SpaceId(0));
    tx.set_cell(b(20, 36, 4), Cell::from_parts(rules::ids::STONE, 0));
    let change = world.commit(tx).unwrap();
    owner.observe(&[change], Tick(1005));
    owner.begin_tick(Tick(1005));
    let (key, _) = owner.next_request().unwrap();
    assert!(
        owner.skip_unchanged(key, &world),
        "unsampled interior doesn't rebuild or advance water view"
    );
    owner.request(1, req, Tick(1006)).unwrap();
    owner.begin_tick(Tick(1006));
    let (key, current) = owner.next_request().unwrap();
    assert!(owner.submit(
        key,
        current,
        Snapshot::capture(&world, key.1).unwrap(),
        Tick(1006)
    ));
    owner.forget(1, req.page);
    owner.request(1, req, Tick(1006)).unwrap();
    drain_job(&mut owner, &world);
    assert_eq!(
        owner.stats(Tick(1006)).queued,
        1,
        "old completion must not consume replacement stream"
    );
    let replacement = build(&mut owner, &world, Tick(1007));
    assert!(replacement.water_view > second.water_view);
}
#[test]
fn ten_thousand_parked_subscriptions_have_bounded_metadata_and_idle_allocates_nothing() {
    let scope = telemetry::AllocationScope::begin().unwrap();
    let mut owner = owner(40, 250);
    for connection in 1..=40 {
        for x in 0..250 {
            owner
                .request(connection, request(p(x, 2, connection as i32)), Tick(0))
                .unwrap();
        }
    }
    let metadata = scope.finish();
    // As in the canonical invalid-binding branch, leave these requests parked
    // until a fresh client binding arrives. No cold World or generated pages.
    for tick in 0..2500 {
        owner.begin_tick(Tick(tick));
        for _ in 0..4 {
            assert!(owner.next_request().is_some());
        }
    }
    assert_eq!(owner.stats(Tick(2500)).streams, 10_000);
    assert_eq!(owner.stats(Tick(2500)).queued, 0);
    assert_eq!(owner.stats(Tick(2500)).watcher_links, 270_000);
    let world = World::new(rules::properties, 16);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for tick in 2500..12500 {
        owner.begin_tick(Tick(tick));
        owner.retain_peers(|_| true);
        owner.wake_retries(Tick(tick));
        assert!(owner.next_request().is_none());
        assert!(owner.poll(&world).is_none());
    }
    let idle = scope.finish();
    assert_eq!(idle.allocating_calls(), 0);
    assert_eq!(idle.dealloc_calls, 0);
    assert_eq!(owner.stats(Tick(12500)).submitted, 0);
    eprintln!(
        "far_water parked10000 metadata_owned={}B /idle10000alloc0 /270000watcherlinks",
        metadata.allocated_bytes - metadata.released_bytes
    );
}
