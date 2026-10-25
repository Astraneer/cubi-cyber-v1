use telemetry::{
    AllocationScope, CountingAllocator,
    flight::{FlightRecorder, Observations, Sample},
};
use ui::{
    app::{Modifiers, Screen, UiApp},
    atlas::UiAtlas,
    paint::PaintList,
    profiler::{GRAPH_BUCKETS, ProfilerSnapshot},
};
#[global_allocator]
static ALLOCATOR: CountingAllocator<std::alloc::System> =
    CountingAllocator::new(std::alloc::System);
#[test]
fn measured_stages_spikes_missing_gpu_and_concurrent_flags_remain_distinct() {
    let mut recorder = FlightRecorder::new(32, 32).unwrap();
    recorder
        .record_frame(Sample {
            frame: 1,
            monotonic_ns: 0,
            interval_ns: 16_000_000,
            cpu_ns: Some(4_000_000),
            observations: Observations(Observations::UPLOAD_BUDGET),
            ..Sample::default()
        })
        .unwrap();
    recorder
        .record_frame(Sample {
            frame: 2,
            monotonic_ns: 9_999_000_000,
            interval_ns: 90_000_000,
            cpu_ns: Some(30_000_000),
            observations: Observations(Observations::GPU_WAIT),
            ..Sample::default()
        })
        .unwrap();
    recorder
        .record_frame(Sample {
            frame: 3,
            monotonic_ns: 10_000_000_000,
            interval_ns: 16_000_000,
            cpu_ns: Some(3_000_000),
            cpu_stages_ns: std::array::from_fn(|i| Some(i as u64 * 1000)),
            gpu_passes_ns: [None; 16],
            queued_jobs: 7,
            in_flight: 2,
            ..Sample::default()
        })
        .unwrap();
    let snapshot = ProfilerSnapshot::from_recorder(&recorder);
    assert!(snapshot.complete_window);
    assert_eq!(snapshot.gpu_ns, None);
    assert_eq!(snapshot.gpu_passes_ns, [None; 16]);
    assert_eq!(snapshot.cpu_stages_ns[7], Some(7000));
    assert_eq!(snapshot.queued_jobs, 7);
    assert_eq!(snapshot.observations.0, 0);
    assert_eq!(
        snapshot.graph[GRAPH_BUCKETS - 1].interval_ns,
        Some(90_000_000)
    );
    assert_eq!(
        snapshot.graph[GRAPH_BUCKETS - 1].observations,
        Observations::GPU_WAIT
    );
    assert_eq!(snapshot.graph[0].observations, Observations::UPLOAD_BUDGET);
    assert_eq!(
        ProfilerSnapshot::from_recorder(&FlightRecorder::new(2, 2).unwrap()),
        ProfilerSnapshot::default()
    );
}

#[test]
fn unchanged_f3_overlay_retains_geometry_and_snapshot_without_allocations() {
    let mut recorder = FlightRecorder::new(32, 32).unwrap();
    recorder
        .record_frame(Sample {
            frame: 1,
            monotonic_ns: 1,
            interval_ns: 16_000_000,
            ..Sample::default()
        })
        .unwrap();
    let snapshot = ProfilerSnapshot::from_recorder(&recorder);
    let atlas = UiAtlas::load().unwrap();
    let mut app = UiApp::new().unwrap();
    app.open(Screen::Game);
    app.key("F3", true, Modifiers::default());
    app.set_profiler(snapshot);
    let mut paint = PaintList::default();
    assert!(app.paint(&atlas, &mut paint, 780, 437));
    assert!(app.show_debug);
    assert!(paint.quads.len() > 300);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        app.set_profiler(ProfilerSnapshot::from_recorder(&recorder));
        assert!(!app.paint(&atlas, &mut paint, 780, 437));
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    let scope = AllocationScope::begin().unwrap();
    std::hint::black_box(vec![0u8; 32]);
    assert!(scope.finish().allocating_calls() > 0);
}
