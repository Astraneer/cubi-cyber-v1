use ui::{
    app::{Intent, Modifiers, Role, Screen, Snapshot, UiApp, Widget},
    atlas::UiAtlas,
    paint::PaintList,
};

fn drain(app: &mut UiApp) -> Vec<Intent> {
    std::iter::from_fn(|| app.take_intent()).collect()
}

#[test]
fn showcase_allows_recipe_inspection_and_close_without_inventory_or_side_mutation() {
    let atlas = UiAtlas::load().unwrap();
    let mut app = UiApp::new().unwrap();
    let mut snapshot = Snapshot {
        connected: true,
        role: Role::Owner,
        window_id: 17,
        window_revision: 3,
        inventory_revision: 6,
        window_read_only: true,
        container_slots: 2,
        energy: 120_000,
        ..Default::default()
    };
    snapshot.container[0] = rules::stack::ItemStack::new(rules::ids::WIND_GENERATOR, 1).unwrap();
    app.set_snapshot(snapshot.clone()).unwrap();
    app.open(Screen::Machine);
    let mut paint = PaintList::default();
    app.paint(&atlas, &mut paint, 780, 437);
    drain(&mut app);
    for slot in [0, 41] {
        let rect = app.hit(Widget::Slot(slot)).unwrap();
        for (right, shift) in [(false, false), (true, false), (false, true)] {
            app.click(
                rect.x + rect.w * 0.5,
                rect.y + rect.h * 0.5,
                right,
                Modifiers {
                    shift,
                    ..Default::default()
                },
            );
            assert!(drain(&mut app).is_empty());
        }
    }
    assert!((0..6).all(|face| app.hit(Widget::Side(face)).is_none()));
    assert!(app.snapshot == snapshot);
    let rect = app.hit(Widget::Slot(41)).unwrap();
    app.pointer(rect.x + 4., rect.y + 4.);
    app.key("KeyR", true, Modifiers::default());
    assert_eq!(app.screen, Screen::Recipe);
    app.paint(&atlas, &mut paint, 780, 437);
    assert!(
        !app.hits()
            .iter()
            .any(|hit| matches!(hit.widget, Widget::RecipePlus(_)))
    );
    assert!(
        !drain(&mut app)
            .iter()
            .any(|intent| matches!(intent, Intent::RecipeFill { .. }))
    );
    app.key("Escape", true, Modifiers::default());
    // Closing remains a reliable authority operation, including from a recipe
    // opened above the read-only machine window.
    app.open(Screen::Game);
    assert!(
        drain(&mut app)
            .iter()
            .any(|intent| matches!(intent, Intent::CloseWindow { window: 17, .. }))
    );
    assert!(app.snapshot == snapshot);
}
