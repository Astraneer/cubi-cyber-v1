use client::quality::QualityRuntime;
use telemetry::flight::Sample;
use ui::settings::{Setting, Settings};

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

fn bit(setting: Setting) -> u32 {
    1 << setting as u32
}
fn window(
    runtime: &mut QualityRuntime,
    next_frame: &mut u64,
    start_ms: u64,
    frame_us: u32,
    gpu_us: u32,
    complete: bool,
) -> u64 {
    let frames = 1_000_000 / frame_us as u64 + 2;
    for i in 0..frames {
        *next_frame += 1;
        runtime.record_present(&Sample {
            frame: *next_frame,
            monotonic_ns: start_ms * 1_000_000 + i * u64::from(frame_us) * 1000,
            interval_ns: u64::from(frame_us) * 1000,
            ..Sample::default()
        });
        runtime.complete_gpu(*next_frame, u64::from(gpu_us) * 1000, complete);
    }
    start_ms + frames * u64::from(frame_us) / 1000
}
#[test]
fn unsupported_levers_are_skipped_and_only_the_actual_backend_receipt_commits_a_proposal() {
    let saved = Settings::default();
    let original = saved.clone();
    let mut runtime = QualityRuntime::new(&saved);
    runtime.set_active(true, 0);
    let mut frame = 0;
    for start in [10_000, 12_000] {
        let now = window(&mut runtime, &mut frame, start, 22_000, 13_000, true);
        assert!(runtime.poll(now, bit(Setting::Reflections)).is_none());
    }
    let now = window(&mut runtime, &mut frame, 14_000, 22_000, 13_000, true);
    let request = runtime.poll(now, bit(Setting::Reflections)).unwrap();
    assert_eq!(request.reduction.setting, Setting::Reflections);
    assert_eq!((request.reduction.before, request.reduction.after), (1, 0));
    assert_eq!(runtime.target().get(Setting::Reflections), 0);
    assert_eq!(runtime.confirmed(Setting::Reflections), 1);
    assert!(!runtime.acknowledge(request.id + 1, 50_000, u32::MAX));
    assert!(!runtime.acknowledge(request.id, 50_000, bit(Setting::Msaa)));
    assert!(runtime.acknowledge(request.id, 50_000, bit(Setting::Reflections)));
    assert_eq!(runtime.confirmed(Setting::Reflections), 0);
    assert_eq!(runtime.stats().applied, 1);
    assert_eq!(saved, original);
    assert_eq!(runtime.confirmed(Setting::Msaa), 4);
    assert_eq!(runtime.confirmed(Setting::Shadows), 1);
    assert_eq!(runtime.confirmed(Setting::Scale), 100);

    // Actual preparation took35 seconds. The cooldown starts at its receipt,
    // and three completed post-change windows measure the saving before return.
    for start in [51_000, 53_000, 55_000, 57_000] {
        let now = window(&mut runtime, &mut frame, start, 12_000, 7_000, true);
        assert!(runtime.poll(now, bit(Setting::Reflections)).is_none());
    }
    assert_eq!(runtime.stats().measured_savings, 1);
    assert_eq!(
        runtime.reductions().next().unwrap().saved_microseconds,
        6_000
    );
    let now = window(&mut runtime, &mut frame, 60_000, 12_000, 7_000, true);
    let restore = runtime.poll(now, bit(Setting::Reflections)).unwrap();
    assert!(restore.restoring);
    assert_eq!((restore.reduction.before, restore.reduction.after), (0, 1));
    assert_eq!(runtime.confirmed(Setting::Reflections), 0);
    assert!(runtime.acknowledge(restore.id, now, bit(Setting::Reflections)));
    assert_eq!(runtime.confirmed(Setting::Reflections), 1);
    assert_eq!(saved, original);
}

#[test]
fn partial_gpu_scopes_loading_stale_frames_and_single_hitches_cannot_change_quality() {
    let saved = Settings::default();
    let mut runtime = QualityRuntime::new(&saved);
    let mut frame = 0;
    // Not a playable view yet.
    let now = window(&mut runtime, &mut frame, 10_000, 22_000, 20_000, true);
    assert!(runtime.poll(now, u32::MAX).is_none());
    runtime.set_active(true, now);
    for start in [20_000, 22_000, 24_000, 26_000] {
        let now = window(&mut runtime, &mut frame, start, 22_000, 20_000, false);
        assert!(runtime.poll(now, u32::MAX).is_none());
    }
    assert_eq!(runtime.stats().complete_frames, 0);
    assert!(runtime.stats().incomplete_frames > 100);
    for start in [30_000, 32_000, 34_000, 36_000] {
        let now = window(&mut runtime, &mut frame, start, 12_000, 7_000, true);
        // A valid one-off hitch does not move the ninety-fifth percentile.
        frame += 1;
        runtime.record_present(&Sample {
            frame,
            monotonic_ns: now * 1_000_000,
            interval_ns: 80_000_000,
            ..Sample::default()
        });
        runtime.complete_gpu(frame, 80_000_000, true);
        runtime.complete_gpu(frame, 80_000_000, true); // Duplicate is ignored.
        assert!(runtime.poll(now, u32::MAX).is_none());
        assert_eq!(runtime.stats().frame_p95_us, 12_000);
    }
    assert_eq!(runtime.stats().proposals, 0);
    assert_eq!(runtime.target(), &saved);
}

#[test]
fn user_edit_or_failed_preparation_invalidates_receipts_and_preserves_confirmed_values() {
    let mut saved = Settings::default();
    let mut runtime = QualityRuntime::new(&saved);
    runtime.set_active(true, 0);
    let mut frame = 0;
    let mut request = None;
    for start in [10_000, 12_000, 14_000] {
        let now = window(&mut runtime, &mut frame, start, 22_000, 13_000, true);
        request = runtime.poll(now, bit(Setting::Reflections));
    }
    let request = request.unwrap();
    assert!(runtime.failed(request.id, 20_000));
    assert_eq!(runtime.target(), &saved);
    assert_eq!(runtime.confirmed(Setting::Reflections), 1);
    assert!(!runtime.acknowledge(request.id, 21_000, u32::MAX));
    let mut request = None;
    for start in [30_000, 32_000, 34_000] {
        let now = window(&mut runtime, &mut frame, start, 22_000, 13_000, true);
        request = runtime.poll(now, bit(Setting::Reflections));
    }
    let request = request.unwrap();
    saved.set(Setting::Reflections, 2).unwrap();
    runtime.update_saved(&saved, 36_000);
    assert_eq!(runtime.target(), &saved);
    assert!(!runtime.acknowledge(request.id, 37_000, u32::MAX));
    assert_eq!(runtime.confirmed(Setting::Reflections), 2);
    assert_eq!(runtime.reductions().count(), 0);
}

#[test]
fn sound_controls_and_interface_changes_keep_confirmed_and_pending_graphics_reductions() {
    let mut saved = Settings::default();
    let mut runtime = QualityRuntime::new(&saved);
    runtime.set_active(true, 0);
    let mut frame = 0;
    let mut request = None;
    for start in [10_000, 12_000, 14_000] {
        let now = window(&mut runtime, &mut frame, start, 22_000, 13_000, true);
        request = runtime.poll(now, bit(Setting::Reflections));
    }
    let request = request.unwrap();
    saved.set(Setting::Master, 50).unwrap();
    saved.set(Setting::Sensitivity, 155).unwrap();
    saved.set(Setting::UiScale, 125).unwrap();
    runtime.update_saved(&saved, 18_000);
    assert_eq!(runtime.pending().unwrap().id, request.id);
    assert_eq!(runtime.target().get(Setting::Reflections), 0);
    assert_eq!(runtime.target().get(Setting::Master), 50);
    assert_eq!(runtime.target().get(Setting::Sensitivity), 155);
    assert_eq!(runtime.target().get(Setting::UiScale), 125);
    assert!(runtime.acknowledge(request.id, 20_000, bit(Setting::Reflections)));
    saved.set(Setting::Master, 25).unwrap();
    runtime.update_saved(&saved, 21_000);
    assert_eq!(runtime.confirmed(Setting::Reflections), 0);
    assert_eq!(runtime.confirmed(Setting::Master), 25);
    assert_eq!(runtime.target().get(Setting::Master), 25);
    assert_eq!(runtime.reductions().count(), 1);
    saved.set(Setting::Exposure, 120).unwrap();
    runtime.update_saved(&saved, 22_000);
    assert_eq!(runtime.confirmed(Setting::Reflections), 1);
    assert_eq!(runtime.target(), &saved);
    assert_eq!(runtime.reductions().count(), 0);
}

#[test]
fn completed_windows_proposals_receipts_and_recovery_allocate_nothing() {
    let saved = Settings::default();
    let mut runtime = QualityRuntime::new(&saved);
    runtime.set_active(true, 0);
    let mut frame = 0;
    let scope = telemetry::AllocationScope::begin().unwrap();
    for i in 0..10_000 {
        let now = window(
            &mut runtime,
            &mut frame,
            10_000 + i * 2_000,
            if i % 32 < 16 { 22_000 } else { 12_000 },
            if i % 32 < 16 { 13_000 } else { 7_000 },
            true,
        );
        if let Some(request) = runtime.poll(now, bit(Setting::Reflections)) {
            assert!(runtime.acknowledge(request.id, now, bit(Setting::Reflections)));
        }
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    assert!(runtime.stats().proposals > 100);
    assert!(runtime.stats().measured_savings > 100);
}
