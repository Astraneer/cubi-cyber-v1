use std::alloc::System;
use std::hint::black_box;
use std::sync::{Arc, Barrier};
use telemetry::{
    AllocationCounts, AllocationScope, CountingAllocator, FrameRecord, FrameRecorder, ScopeError,
    WorkCounters, WorkCounts,
};

#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);

#[test]
fn records_allocation_reallocation_release_and_bytes() {
    let scope = AllocationScope::begin().unwrap();
    let mut bytes = Vec::<u8>::with_capacity(17);
    let original_capacity = bytes.capacity() as u64;
    bytes.extend_from_slice(black_box(&[5; 17]));
    bytes.reserve_exact(35);
    let grown_capacity = bytes.capacity() as u64;
    black_box(bytes.as_slice());
    drop(bytes);
    let counts = scope.finish();

    assert_eq!(counts.alloc_calls, 1);
    assert_eq!(counts.realloc_calls, 1);
    assert_eq!(counts.dealloc_calls, 1);
    assert_eq!(counts.failed_calls, 0);
    assert_eq!(counts.allocated_bytes, original_capacity + grown_capacity);
    assert_eq!(counts.released_bytes, original_capacity + grown_capacity);
}

#[test]
fn records_zero_initialization() {
    let scope = AllocationScope::begin().unwrap();
    let bytes = vec![0_u8; black_box(257)];
    black_box(bytes.as_slice());
    drop(bytes);
    let counts = scope.finish();

    assert_eq!(counts.zeroed_alloc_calls, 1);
    assert_eq!(counts.dealloc_calls, 1);
    assert_eq!(counts.allocated_bytes, 257);
    assert_eq!(counts.released_bytes, 257);
}

#[test]
fn static_recording_loop_does_no_allocating_compiling_or_generating() {
    let mut recorder = FrameRecorder::<600>::new();
    let mut counters = WorkCounters::new();
    let scope = AllocationScope::begin().unwrap();
    for frame in 0..10_000 {
        counters.record(WorkCounts::ZERO);
        recorder.push(FrameRecord {
            frame,
            work: counters.take(),
            ..FrameRecord::EMPTY
        });
        black_box(
            recorder
                .iter()
                .map(|record| record.work.upload_bytes)
                .sum::<u64>(),
        );
    }
    let counts = scope.finish();
    assert_eq!(counts, AllocationCounts::ZERO);
    assert_eq!(recorder.len(), 600);
    assert_eq!(recorder.iter().next().unwrap().frame, 9400);
    assert!(
        recorder
            .iter()
            .all(|record| record.work.pipeline_compilations == 0
                && record.work.synchronous_generations == 0)
    );
}

#[test]
fn another_threads_allocations_do_not_contaminate_this_scope() {
    let barrier = Arc::new(Barrier::new(2));
    let other_barrier = Arc::clone(&barrier);
    let thread = std::thread::spawn(move || {
        other_barrier.wait();
        let bytes = vec![42_u8; black_box(8192)];
        black_box(bytes);
        other_barrier.wait();
    });
    let scope = AllocationScope::begin().unwrap();
    barrier.wait();
    barrier.wait();
    let counts = scope.finish();
    thread.join().unwrap();
    assert_eq!(counts, AllocationCounts::ZERO);
}

#[test]
fn nested_scope_does_not_reset_outer_measurement() {
    let outer = AllocationScope::begin().unwrap();
    assert!(matches!(
        AllocationScope::begin(),
        Err(ScopeError::AlreadyActive)
    ));
    let data = vec![1_u8; black_box(37)];
    black_box(data);
    let counts = outer.finish();
    assert_eq!(counts.allocating_calls(), 1);
}

#[test]
fn dropping_guard_restores_measurement_after_unwind() {
    let panic = std::panic::catch_unwind(|| {
        let _scope = AllocationScope::begin().unwrap();
        panic!("intentional scope-unwind test");
    });
    assert!(panic.is_err());
    let scope = AllocationScope::begin().unwrap();
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}
