extern crate net as protocol;
extern crate self as net;
pub const MAX_COORDINATE: i32 = protocol::MAX_COORDINATE;
#[path = "../src/ui_window.rs"]
mod bridge;
#[path = "../src/network/window.rs"]
mod cache;
#[path = "../../net/src/window.rs"]
pub mod window;
use window::*;
fn state() -> WindowSnapshot {
    WindowSnapshot {
        guard: WindowGuard {
            id: 7,
            revision: 1,
            inventory_revision: 4,
        },
        kind: WindowKind::Inventory,
        closed: false,
        binding: None,
        cursor: Stack::EMPTY,
        craft: [Stack::EMPTY; 9],
        craft_result: Stack::EMPTY,
        container: vec![],
        progress: 0,
        fuel: 0,
        energy: 0,
        sides: [0; 6],
    }
}
#[test]
fn stale_epochs_and_mismatched_inventory_cannot_supply_actionable_windows() {
    let mut cache = cache::WindowCache::default();
    let first = state();
    assert!(cache.receive(first.clone()).unwrap());
    assert_eq!(cache.latest(), Some(&first));
    assert!(cache.matching_inventory(3).is_none());
    assert!(cache.matching_inventory(4).is_some());
    assert!(!cache.receive(first.clone()).unwrap());
    let mut stale = first.clone();
    stale.guard.id = 6;
    assert!(!cache.receive(stale).unwrap());
    assert_eq!(
        cache.counters(),
        cache::WindowCounters {
            updates: 1,
            stale: 1
        }
    );
    cache.clear();
    assert!(cache.latest().is_none());
}
#[test]
fn ui_projection_is_registry_validated_atomic_and_preserves_server_counts() {
    let mut ui = ui::app::UiApp::new().unwrap();
    let mut snapshot = ui::app::Snapshot {
        connected: true,
        inventory_revision: 4,
        ..Default::default()
    };
    ui.set_snapshot(snapshot.clone()).unwrap();
    let mut image = state();
    image.cursor = Stack {
        item: rules::items::block_item(rules::ids::STONE),
        count: 3,
        damage: 0,
        payload: 0,
    };
    assert!(bridge::apply_window(&mut ui, &image).unwrap());
    assert_eq!(ui.screen, ui::app::Screen::Inventory);
    assert_eq!(ui.snapshot.cursor.count(), 3);
    snapshot = ui.snapshot.clone();
    image.cursor.item = u16::MAX;
    assert!(bridge::apply_window(&mut ui, &image).is_err());
    assert!(ui.snapshot == snapshot);
    image = state();
    image.guard.inventory_revision = 5;
    assert!(bridge::apply_window(&mut ui, &image).is_err());
    assert!(ui.snapshot == snapshot);
    image = state();
    image.closed = true;
    image.guard.revision = 2;
    bridge::apply_window(&mut ui, &image).unwrap();
    assert_eq!(ui.screen, ui::app::Screen::Game);
    assert_eq!(ui.snapshot.window_id, 0);
    assert!(ui.snapshot.cursor.is_empty());
}

#[test]
fn window_payload_survives_cursor_carry_without_disclosing_private_contents() {
    let payload = rules::item_payload::Payload::charge(120_000).unwrap();
    let mut ui = ui::app::UiApp::new().unwrap();
    ui.set_snapshot(ui::app::Snapshot {
        connected: true,
        inventory_revision: 4,
        ..Default::default()
    })
    .unwrap();
    let mut image = state();
    image.cursor = Stack {
        item: rules::items::I_ENERGY_TABLET,
        count: 1,
        damage: 0,
        payload: payload.id().0,
    };
    let image = decode_snapshot(&encode_snapshot(&image).unwrap()).unwrap();
    bridge::apply_window(&mut ui, &image).unwrap();
    assert_eq!(ui.snapshot.cursor.payload(), Some(payload.id()));
    assert_eq!(ui.snapshot.cursor.count(), 1);
    let mut moved = image.clone();
    moved.guard.revision += 1;
    moved.cursor = Stack::EMPTY;
    moved.craft[0] = image.cursor;
    bridge::apply_window(&mut ui, &moved).unwrap();
    assert!(ui.snapshot.cursor.is_empty());
    assert_eq!(ui.snapshot.craft[0].payload(), Some(payload.id()));
    let atlas = ui::atlas::UiAtlas::load().unwrap();
    let mut paint = ui::paint::PaintList::default();
    ui.paint(&atlas, &mut paint, 780, 437);
    let rect = ui.hit(ui::app::Widget::Slot(100)).unwrap();
    ui.pointer(rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
    assert_eq!(ui.hovered_state_label(), Some("Сохранённый заряд"));
    assert!(ui.paint(&atlas, &mut paint, 780, 437));
    assert!(
        paint
            .quads
            .iter()
            .any(|quad| quad.color == 0xf0181b23 && quad.rect.h == 44.0)
    );
    let before = ui.snapshot.clone();
    moved.craft[0].count = 2;
    assert!(bridge::apply_window(&mut ui, &moved).is_err());
    assert!(ui.snapshot == before);
}
