use std::{alloc::System, hint::black_box};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator, flight::*};
#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);
#[test]
fn history_is_ten_seconds_at_variable_frame_rates_and_json_keeps_unknowns() {
    let mut recorder = FlightRecorder::new(4096, 256).unwrap();
    let mut expected = Vec::new();
    let mut sequence = 0;
    for frame in 0..1600_u64 {
        let now = if frame < 300 {
            frame * 16_666_667
        } else {
            5_000_000_100 + (frame - 300) * 6_944_445
        };
        if frame % 10 == 0 {
            sequence += 1;
            recorder
                .record_input(ReplayInput {
                    monotonic_ns: now,
                    tick: now / 50_000_000,
                    sequence,
                    ..ReplayInput::default()
                })
                .unwrap();
        }
        let sample = Sample {
            frame: frame + 1,
            monotonic_ns: now,
            interval_ns: if frame < 300 { 16_666_667 } else { 6_944_445 },
            cpu_ns: Some(123),
            ..Sample::default()
        };
        recorder.record_frame(sample).unwrap();
        expected.push(sample);
    }
    let end = expected.last().unwrap().monotonic_ns;
    let kept: Vec<_> = expected
        .into_iter()
        .filter(|s| s.monotonic_ns >= end - WINDOW_NS)
        .collect();
    assert_eq!(recorder.frames().copied().collect::<Vec<_>>(), kept);
    assert!(recorder.complete_window());
    assert_eq!(recorder.loss(), Loss::default());
    let value: serde_json::Value =
        serde_json::from_str(&recorder.export_json("wgpu\"\n\\", u64::MAX, 0xabc, 40)).unwrap();
    assert_eq!(value["backend"], "wgpu\"\n\\");
    assert_eq!(value["publicSeed"], "ffffffffffffffff");
    assert!(value["frames"][0]["gpuNs"].is_null());
    assert!(value["frames"][0]["allocations"].is_null());
    assert_eq!(value["cpuStages"].as_array().unwrap().len(), 8);
    assert_eq!(value["gpuPasses"].as_array().unwrap().len(), 16);
}
#[test]
fn capacity_loss_and_warmup_are_reported_instead_of_claiming_a_full_window() {
    let mut recorder = FlightRecorder::new(3, 3).unwrap();
    assert!(!recorder.complete_window());
    for frame in 0..10 {
        recorder
            .record_frame(Sample {
                frame,
                monotonic_ns: frame * 1_000_000,
                ..Sample::default()
            })
            .unwrap();
    }
    assert_eq!(recorder.loss().frames, 7);
    assert!(!recorder.complete_window());
    recorder
        .record_frame(Sample {
            frame: 20,
            monotonic_ns: WINDOW_NS + 20_000_000,
            ..Sample::default()
        })
        .unwrap();
    assert!(recorder.complete_window());
    assert_eq!(recorder.frames().len(), 1);
}
#[test]
fn invalid_records_are_atomic_and_observations_are_explicit() {
    let mut recorder = FlightRecorder::new(20, 20).unwrap();
    recorder
        .record_frame(Sample {
            frame: 1,
            monotonic_ns: 100,
            allocations: Some(AllocationCounts {
                zeroed_alloc_calls: 1,
                ..AllocationCounts::ZERO
            }),
            ..Sample::default()
        })
        .unwrap();
    assert_eq!(
        recorder.frames().next().unwrap().observations.0,
        Observations::ALLOCATION
    );
    let before = recorder.export_json("Vulkan", 1337, 0, 0);
    assert_eq!(
        recorder.record_frame(Sample {
            frame: 2,
            monotonic_ns: 99,
            ..Sample::default()
        }),
        Err(RecordError::ClockWentBackwards)
    );
    assert_eq!(
        recorder.record_input(ReplayInput {
            monotonic_ns: 101,
            yaw_bits: f32::NAN.to_bits(),
            ..ReplayInput::default()
        }),
        Err(RecordError::InvalidInput)
    );
    assert_eq!(
        recorder.record_frame(Sample {
            frame: 2,
            monotonic_ns: 101,
            in_flight: 3,
            ..Sample::default()
        }),
        Err(RecordError::InvalidInput)
    );
    assert_eq!(before, recorder.export_json("Vulkan", 1337, 0, 0));
}
#[test]
fn steady_recording_has_no_allocation_or_deallocation_with_positive_control() {
    let mut recorder = FlightRecorder::new(2048, 256).unwrap();
    let scope = AllocationScope::begin().unwrap();
    for frame in 1..=20000 {
        recorder
            .record_frame(Sample {
                frame,
                monotonic_ns: frame * 8_000_000,
                ..Sample::default()
            })
            .unwrap();
        if frame % 6 == 0 {
            recorder
                .record_input(ReplayInput {
                    monotonic_ns: frame * 8_000_000,
                    tick: frame / 6,
                    sequence: frame / 6,
                    ..ReplayInput::default()
                })
                .unwrap();
        }
    }
    let counts = scope.finish();
    black_box(&recorder);
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    let scope = AllocationScope::begin().unwrap();
    let value = black_box(Box::new([1_u8; 128]));
    drop(value);
    let control = scope.finish();
    assert!(control.allocating_calls() > 0);
    assert!(control.dealloc_calls > 0);
}
