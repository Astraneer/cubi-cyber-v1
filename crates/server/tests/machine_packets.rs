use core_types::{BlockPos, ComponentId, SpaceId, Tick, Version};
use net::machine_packets::{ACTIVE, Request};
use rules::{items::I_IRON_INGOT, stack::ItemStack};
use server::machines::{replication::PacketHistory, transport::*, *};
use telemetry::{AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn p(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 2,
        z: 2,
    }
}
fn packet(id: u64, x: i32) -> Packet {
    Packet {
        id,
        stack: ItemStack::new(I_IRON_INGOT, 1).unwrap(),
        color: 0,
        from: p(x - 1),
        path: vec![p(x), p(x + 1)],
        step: 0,
        progress: 5,
        goal: Goal::Destination,
        home: None,
        idle_direction: None,
        destination: Some(ComponentId(2)),
        last_tick: Tick(1),
        due: Tick(2),
        first_due: Tick(2),
    }
}
fn runtime(count: u64) -> MachineRuntime {
    let mut snapshot = MachineRuntime::new(Config::default()).unwrap().snapshot();
    snapshot.transport.next_packet = count + 1;
    for id in 1..=count {
        let row = packet(id, (id as i32 - 1) * 16 + 2);
        snapshot.transport.clocks.push(ClockRecord {
            page: row.at().unwrap().page(),
            clock: PageClock::default(),
        });
        snapshot.transport.packets.push(row);
    }
    MachineRuntime::restore(Config::default(), &snapshot).unwrap()
}
fn advance(runtime: &mut MachineRuntime, progress: u8, last_tick: Tick) {
    let before = runtime.packet(1).unwrap().clone();
    let mut after = before.clone();
    after.progress = progress;
    after.last_tick = last_tick;
    after.due = Tick(last_tick.0 + 1);
    let mut transport = runtime.transport_delta();
    transport.packets.push(PacketPatch {
        before: Some(before),
        after: Some(after),
    });
    let mut delta = runtime.noop_delta().unwrap();
    delta.transport = Some(transport);
    runtime.acknowledge(delta).unwrap();
}
#[test]
fn packet_history_uses_confirmed_base_suppresses_linear_motion_and_recovers_full() {
    let mut runtime = runtime(1);
    let page = p(2).page();
    let mut history = PacketHistory::new(page, Version(7), 8, 2).unwrap();
    let (full, work) = history.plan(&runtime, Tick(1)).unwrap();
    let full = full.unwrap();
    assert!(work.full);
    assert_eq!(full.base, None);
    assert_eq!(full.upserts[0].flags, ACTIVE);
    assert!(history.request(Request::Ack {
        page,
        view_version: Version(7),
        revision: 1
    }));
    advance(&mut runtime, 10, Tick(2));
    assert!(history.plan(&runtime, Tick(2)).unwrap().0.is_none());
    let delta = runtime
        .extend_page_activity(runtime.noop_delta().unwrap(), &[(page, false)], Tick(3))
        .unwrap();
    runtime.acknowledge(delta).unwrap();
    let frozen = history.plan(&runtime, Tick(3)).unwrap().0.unwrap();
    assert_eq!(frozen.base, Some(1));
    assert_eq!(frozen.upserts[0].flags, 0);
    // The frozen frame was sent, but its ACK was lost. Resume must still use
    // confirmed revision1, not the most recently sent revision2.
    let delta = runtime
        .extend_page_activity(runtime.noop_delta().unwrap(), &[(page, true)], Tick(100))
        .unwrap();
    runtime.acknowledge(delta).unwrap();
    advance(&mut runtime, 15, Tick(3));
    let resumed = history.plan(&runtime, Tick(100)).unwrap().0.unwrap();
    assert_eq!(resumed.base, Some(1));
    assert_eq!(resumed.upserts[0].epoch, Tick(100));
    assert_eq!(resumed.upserts[0].flags, ACTIVE);
    assert!(!history.request(Request::Ack {
        page,
        view_version: Version(7),
        revision: 999
    }));
    assert!(history.request(Request::Full {
        page,
        view_version: Version(7),
        have: Some(1)
    }));
    assert!(
        history
            .plan(&runtime, Tick(100))
            .unwrap()
            .0
            .unwrap()
            .base
            .is_none()
    );
    history.set_view_version(Version(8));
    assert!(!history.request(Request::Ack {
        page,
        view_version: Version(7),
        revision: 3
    }));
    let next = history.plan(&runtime, Tick(100)).unwrap().0.unwrap();
    assert_eq!(next.view_version, Version(8));
    assert!(next.base.is_none());
}
#[test]
fn packet_replication_visits_one_indexed_page_among_ten_thousand_without_path_clones() {
    let runtime = runtime(10_000);
    let mut history = PacketHistory::new(p(2).page(), Version(7), 512, 2).unwrap();
    let scope = AllocationScope::begin().unwrap();
    let (frame, work) = history.plan(&runtime, Tick(1)).unwrap();
    let counts = scope.finish();
    assert_eq!(work.indexed_rows, 1);
    assert_eq!(frame.unwrap().upserts.len(), 1);
    assert!(counts.allocated_bytes < 64 * 1024, "{counts:?}");
}
#[test]
fn overflow_and_future_ack_do_not_publish_partial_public_page() {
    let mut snapshot = runtime(1).snapshot();
    snapshot.transport.next_packet = 3;
    snapshot.transport.packets.push(packet(2, 4));
    let runtime2 = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let mut history = PacketHistory::new(p(2).page(), Version(7), 1, 2).unwrap();
    assert!(history.plan(&runtime2, Tick(1)).is_err());
    assert_eq!(history.confirmed(), None);
    assert!(!history.request(Request::Ack {
        page: p(2).page(),
        view_version: Version(7),
        revision: 1
    }));
    let full = history.plan(&runtime(1), Tick(1)).unwrap().0.unwrap();
    assert_eq!(full.revision, 1);
    assert_eq!(full.upserts.len(), 1);
}

#[test]
fn unchanged_linear_public_motion_has_zero_allocations_for_five_hundred_twelve_rows() {
    let mut snapshot = runtime(1).snapshot();
    snapshot.transport.next_packet = 513;
    snapshot.transport.packets = (1..=512).map(|id| packet(id, 2)).collect();
    let mut runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let page = p(2).page();
    let mut history = PacketHistory::new(page, Version(7), 512, 2).unwrap();
    history.plan(&runtime, Tick(1)).unwrap();
    history.request(Request::Ack {
        page,
        view_version: Version(7),
        revision: 1,
    });
    let mut td = runtime.transport_delta();
    for id in 1..=512 {
        let before = runtime.packet(id).unwrap().clone();
        let mut after = before.clone();
        after.progress = 10;
        after.last_tick = Tick(2);
        after.due = Tick(3);
        td.packets.push(PacketPatch {
            before: Some(before),
            after: Some(after),
        });
    }
    let mut delta = runtime.noop_delta().unwrap();
    delta.transport = Some(td);
    runtime.acknowledge(delta).unwrap();
    let scope = AllocationScope::begin().unwrap();
    let (frame, work) = history
        .plan_filtered(&runtime, Tick(2), &|_| true, 512, 8192)
        .unwrap();
    let counts = scope.finish();
    assert!(frame.is_none());
    assert_eq!(work.indexed_rows, 512);
    assert_eq!(counts, telemetry::AllocationCounts::ZERO);
}
#[test]
fn filtered_public_rows_and_probe_budget_fail_closed_without_consuming_revision() {
    let mut snapshot = runtime(1).snapshot();
    snapshot.transport.next_packet = 3;
    snapshot.transport.packets.push(packet(2, 4));
    let runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let mut history = PacketHistory::new(p(2).page(), Version(7), 512, 2).unwrap();
    assert!(
        history
            .plan_filtered(&runtime, Tick(1), &|_| true, 512, 1)
            .is_err()
    );
    let frame = history
        .plan_filtered(&runtime, Tick(1), &|at| at == p(2), 512, 8)
        .unwrap()
        .0
        .unwrap();
    assert_eq!(frame.revision, 1);
    assert_eq!(frame.upserts.len(), 1);
    assert_eq!(frame.upserts[0].id, 1);
}
