use client::{
    network::{Connection, NetworkConfig},
    ui_bridge::UiBridge,
};
use core_types::{BlockPos, SpaceId, Tick};
use net::{ActionId, PlayerState, ServerMessage, Welcome, window::*};
use ui::app::{Intent, Modifiers, Screen, UiApp, Widget};

fn receive(connection: &mut Connection, message: ServerMessage) {
    let bytes = net::encode_server(&message).unwrap();
    connection
        .receive(net::decode_server(&bytes).unwrap(), 10)
        .unwrap();
}

#[test]
fn inventory_arriving_before_window_refresh_does_not_unlock_a_read_only_display() {
    let mut connection = Connection::new(NetworkConfig::default()).unwrap();
    receive(
        &mut connection,
        ServerMessage::Welcome(Welcome {
            session: 1,
            identity: Default::default(),
            new_credential: None,
            role: net::Role::Player,
            tick: Tick(0),
            next_action: ActionId(1),
            public_seed: 1,
            generator_version: terrain_gen::GENERATOR_VERSION,
            rules_version: net::RULES_VERSION,
            player: PlayerState {
                health: 20.,
                ..Default::default()
            },
        }),
    );
    let mut inventory = rules::survival::Inventory::empty();
    receive(
        &mut connection,
        ServerMessage::Inventory {
            data: inventory.encode().to_vec(),
        },
    );
    let mut image = WindowSnapshot {
        guard: WindowGuard {
            id: 5,
            revision: 1,
            inventory_revision: inventory.revision(),
        },
        kind: WindowKind::Chest(27),
        closed: false,
        read_only: true,
        binding: Some(BlockPos {
            space: SpaceId(0),
            x: 0,
            y: 64,
            z: 0,
        }),
        cursor: Stack::EMPTY,
        craft: [Stack::EMPTY; 9],
        craft_result: Stack::EMPTY,
        container: vec![Stack::EMPTY; 27],
        progress: 0,
        fuel: 0,
        energy: 0,
        sides: [0; 6],
    };
    receive(
        &mut connection,
        ServerMessage::Window(Box::new(image.clone())),
    );
    let mut app = UiApp::new().unwrap();
    let mut bridge = UiBridge::default();
    bridge.sync_authority(&mut app, &connection).unwrap();
    assert!(app.snapshot.window_read_only);
    assert_eq!(app.screen, Screen::Container);
    while app.take_intent().is_some() {}
    inventory
        .set_trusted(
            0,
            rules::stack::ItemStack::new(rules::ids::STONE, 1).unwrap(),
        )
        .unwrap();
    receive(
        &mut connection,
        ServerMessage::Inventory {
            data: inventory.encode().to_vec(),
        },
    );
    bridge.sync_authority(&mut app, &connection).unwrap();
    assert!(connection.window().is_none());
    assert_eq!(app.snapshot.window_id, 0);
    assert!(app.snapshot.window_read_only);
    let atlas = ui::atlas::UiAtlas::load().unwrap();
    let mut paint = ui::paint::PaintList::default();
    app.paint(&atlas, &mut paint, 780, 437);
    let rect = app.hit(Widget::Slot(41)).unwrap();
    app.click(rect.x + 4., rect.y + 4., false, Modifiers::default());
    assert!(app.take_intent().is_none());
    // A subsequent coherent inventory window legitimately clears the display
    // lock and exposes the player's own guarded slot operations again.
    image.guard = WindowGuard {
        id: 6,
        revision: 1,
        inventory_revision: inventory.revision(),
    };
    image.kind = WindowKind::Inventory;
    image.binding = None;
    image.read_only = false;
    image.container.clear();
    receive(&mut connection, ServerMessage::Window(Box::new(image)));
    bridge.sync_authority(&mut app, &connection).unwrap();
    assert!(!app.snapshot.window_read_only);
    app.paint(&atlas, &mut paint, 780, 437);
    while app.take_intent().is_some() {}
    let rect = app.hit(Widget::Slot(0)).unwrap();
    app.click(rect.x + 4., rect.y + 4., false, Modifiers::default());
    assert!(matches!(
        app.take_intent(),
        Some(Intent::SlotClick { window: 6, .. })
    ));
}
