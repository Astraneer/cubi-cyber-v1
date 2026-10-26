#[path = "../src/topology_queue.rs"]
mod topology_queue;
use core_types::{BlockPos, SpaceId, Tick};
use topology_queue::*;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn pos(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 2,
        z: 3,
    }
}
#[test]
fn wake_retry_complete_and_checkpoint_preserve_age_order_and_exactly_once() {
    let mut q = Queue::new(8).unwrap();
    assert!(q.plan().unwrap().is_empty());
    let d = q
        .extend_registration(q.plan().unwrap(), [pos(2), pos(1), pos(2)], Tick(10))
        .unwrap();
    q.acknowledge(d).unwrap();
    assert_eq!(q.due(Tick(9), 8).count(), 0);
    assert_eq!(
        q.due(Tick(10), 8).map(|e| e.position).collect::<Vec<_>>(),
        [pos(2), pos(1)]
    );
    let d = q
        .extend_retry(q.plan().unwrap(), [pos(2)], Tick(20))
        .unwrap();
    let bytes = d.encode().unwrap();
    q.acknowledge(Delta::decode(&bytes).unwrap()).unwrap();
    assert_eq!(q.get(pos(2)).unwrap().first_due, Tick(10));
    assert_eq!(q.due(Tick(10), 8).next().unwrap().position, pos(1));
    let d = q
        .extend_registration(q.plan().unwrap(), [pos(2)], Tick(15))
        .unwrap();
    q.acknowledge(d).unwrap();
    assert_eq!(q.get(pos(2)).unwrap().due, Tick(15));
    assert_eq!(q.get(pos(2)).unwrap().order, 1);
    let checkpoint = q.snapshot().encode().unwrap();
    let mut r = Queue::restore(8, &Snapshot::decode(&checkpoint, 8).unwrap()).unwrap();
    let d = q.extend_completion(q.plan().unwrap(), [pos(2)]).unwrap();
    let wal = d.encode().unwrap();
    q.acknowledge(d).unwrap();
    r.acknowledge(Delta::decode(&wal).unwrap()).unwrap();
    assert_eq!(q.snapshot(), r.snapshot());
    assert_eq!(r.len(), 1);
    assert_eq!(r.revision(), 4);
    assert_eq!(r.get(pos(2)), None);
    let before = r.snapshot();
    assert!(r.acknowledge(Delta::decode(&wal).unwrap()).is_err());
    assert_eq!(r.snapshot(), before);
}
#[test]
fn quotas_stale_aba_and_malformed_codec_never_change_confirmed_queue() {
    let mut q = Queue::new(2).unwrap();
    assert!(
        q.extend_registration(q.plan().unwrap(), [pos(0), pos(1), pos(2)], Tick(1))
            .is_err()
    );
    assert!(q.is_empty());
    q.acknowledge(
        q.extend_registration(q.plan().unwrap(), [pos(0)], Tick(1))
            .unwrap(),
    )
    .unwrap();
    let stale = q.extend_completion(q.plan().unwrap(), [pos(0)]).unwrap();
    q.acknowledge(stale.clone()).unwrap();
    q.acknowledge(
        q.extend_registration(q.plan().unwrap(), [pos(0)], Tick(2))
            .unwrap(),
    )
    .unwrap();
    let before = q.snapshot();
    assert!(q.acknowledge(stale).is_err());
    assert_eq!(q.snapshot(), before);
    let mut bad = q
        .extend_retry(q.plan().unwrap(), [pos(0)], Tick(3))
        .unwrap();
    bad.patches[0].after.as_mut().unwrap().order = 999;
    assert!(bad.encode().is_err());
    assert!(q.validate(&bad).is_err());
    let bytes = q.snapshot().encode().unwrap();
    for n in 0..bytes.len() {
        assert!(Snapshot::decode(&bytes[..n], 2).is_err());
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(Snapshot::decode(&trailing, 2).is_err());
    let delta = q
        .extend_retry(q.plan().unwrap(), [pos(0)], Tick(4))
        .unwrap()
        .encode()
        .unwrap();
    for n in 0..delta.len() {
        assert!(Delta::decode(&delta[..n]).is_err());
    }
}
#[test]
fn frozen_empty_snapshot_has_no_unbounded_count_or_hidden_clock() {
    let fixture = [
        84, 80, 81, 49, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    assert_eq!(Snapshot::default().encode().unwrap(), fixture);
    assert_eq!(Snapshot::decode(&fixture, 1).unwrap(), Snapshot::default());
    let mut huge = fixture;
    huge[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Snapshot::decode(&huge, MAX_PENDING).is_err());
}
#[test]
fn hundred_thousand_cold_rows_do_not_get_cloned_on_one_bounded_retry() {
    let mut q = Queue::new(100_000).unwrap();
    for start in (0..100_000).step_by(4096) {
        let end = (start + 4096).min(100_000);
        let d = q
            .extend_registration(q.plan().unwrap(), (start..end).map(pos), Tick(100))
            .unwrap();
        q.acknowledge(d).unwrap();
    }
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        std::hint::black_box(q.due(Tick(0), 64).count());
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    let scope = telemetry::AllocationScope::begin().unwrap();
    let d = q
        .extend_retry(q.plan().unwrap(), (0..64).map(pos), Tick(101))
        .unwrap();
    let wal = d.encode().unwrap();
    q.acknowledge(d).unwrap();
    let counts = scope.finish();
    assert!(
        counts.allocated_bytes < 64 * 1024,
        "bounded retry allocated {}",
        counts.allocated_bytes
    );
    assert!(wal.len() < 8192);
    assert_eq!(q.len(), 100_000);
    assert_eq!(q.get(pos(99_999)).unwrap().due, Tick(100));
    assert_eq!(q.get(pos(0)).unwrap().first_due, Tick(100));
}
