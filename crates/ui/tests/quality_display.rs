use ui::{
    app::{Modifiers, Screen, UiApp},
    atlas::UiAtlas,
    paint::{PaintList, TextBuffer},
    quality::QualityHud,
    settings::Setting,
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
#[test]
fn quality_display_keeps_unmeasured_savings_and_pending_receipts_explicit() {
    let mut hud = QualityHud {
        attached: true,
        active: true,
        saved: [1; 10],
        confirmed: [0, 1, 1, 1, 1, 1, 1, 1, 1, 1],
        pending: Some(Setting::Gi),
        ..Default::default()
    };
    let mut text = TextBuffer::default();
    hud.write_text(&mut text);
    assert!(text.as_str().contains("SSR1→0 ?мс"));
    assert!(text.as_str().contains("готовится Gi"));
    assert!(text.as_str().contains("p95 не измерено"));
    hud.savings_us[0] = Some(1250);
    hud.windows = 8;
    hud.frame_p95_us = 20000;
    hud.gpu_p95_us = 18450;
    hud.pending = None;
    hud.write_text(&mut text);
    assert!(text.as_str().contains("SSR1→0 1.25мс"));
    assert!(text.as_str().contains("20.00/18.45мс"));
    assert!(!text.as_str().contains("готовится"));
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10000 {
        hud.write_text(&mut text);
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
}
#[test]
fn unchanged_quality_hud_does_not_rebuild_ui_and_hidden_updates_do_not_upload() {
    let atlas = UiAtlas::load().unwrap();
    let mut app = UiApp::new().unwrap();
    let mut paint = PaintList::default();
    app.open(Screen::Game);
    app.paint(&atlas, &mut paint, 780, 437);
    let revision = app.revision();
    let hud = QualityHud {
        attached: true,
        active: true,
        saved: [1; 10],
        confirmed: [0, 1, 1, 1, 1, 1, 1, 1, 1, 1],
        ..Default::default()
    };
    app.set_quality(hud);
    assert_eq!(app.revision(), revision);
    assert!(!app.paint(&atlas, &mut paint, 780, 437));
    app.key("F3", true, Modifiers::default());
    assert!(app.paint(&atlas, &mut paint, 780, 437));
    let quads = paint.quads.len();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10000 {
        app.set_quality(hud);
        assert!(!app.paint(&atlas, &mut paint, 780, 437));
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    assert_eq!(quads, paint.quads.len());
    app.set_quality(QualityHud {
        confirmed: [0, 0, 1, 0, 1, 1, 1, 1, 1, 1],
        ..hud
    });
    assert!(app.paint(&atlas, &mut paint, 780, 437));
}
