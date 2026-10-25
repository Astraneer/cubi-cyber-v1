use client::{
    network::{Connection, NetworkConfig},
    ui_bridge::{self, Dispatch, UiBridge},
    view::InputKey,
};
use core_types::Tick;
use net::{Action, ActionId, ClientMessage, PlayerId, PlayerState, Role, ServerMessage, Welcome};
use ui::{
    app::{Intent, Modifiers, Screen, Snapshot, UiApp},
    settings::{Setting, Settings},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn connection() -> Connection {
    let mut network = Connection::new(NetworkConfig::default()).unwrap();
    network
        .receive(
            ServerMessage::Welcome(Welcome {
                session: 1,
                identity: PlayerId([1; 16]),
                new_credential: None,
                role: Role::Player,
                tick: Tick(0),
                next_action: ActionId(1),
                public_seed: 1,
                generator_version: terrain_gen::GENERATOR_VERSION,
                rules_version: net::RULES_VERSION,
                player: PlayerState {
                    identity: PlayerId([1; 16]),
                    health: 20.0,
                    ..PlayerState::default()
                },
            }),
            0,
        )
        .unwrap();
    network
}
#[test]
fn menu_and_chat_capture_release_world_keys_and_mouse_settings_are_shared() {
    let mut ui = UiApp::new().unwrap();
    let route = ui_bridge::route_key(&mut ui, "KeyW", true, Modifiers::default());
    assert!(route.captured && route.clear_world && route.world.is_none());
    ui.set_snapshot(Snapshot {
        connected: true,
        ..Snapshot::default()
    })
    .unwrap();
    ui.open(Screen::Game);
    assert_eq!(
        ui_bridge::route_key(&mut ui, "KeyW", true, Modifiers::default()).world,
        Some((InputKey::Forward, true))
    );
    let chat = ui_bridge::route_key(&mut ui, "KeyT", true, Modifiers::default());
    assert!(chat.clear_world && chat.captured);
    assert_eq!(
        ui_bridge::route_key(&mut ui, "KeyW", false, Modifiers::default()).world,
        Some((InputKey::Forward, false))
    );
    let mut settings = Settings::default();
    settings.set(Setting::Sensitivity, 150).unwrap();
    settings.set(Setting::InvertY, 1).unwrap();
    assert_eq!(
        ui_bridge::mouse_delta(&settings, 10.0, 20.0),
        Some([15.0, -30.0])
    );
    assert_eq!(ui_bridge::mouse_delta(&settings, f64::NAN, 0.0), None);
}
#[test]
fn authority_is_cached_without_frame_allocations_and_disconnect_clears_old_items() {
    let mut network = connection();
    let mut inventory = rules::survival::Inventory::empty();
    inventory.select(3).unwrap();
    network
        .receive(
            ServerMessage::Inventory {
                data: inventory.encode().to_vec(),
            },
            0,
        )
        .unwrap();
    let mut bridge = UiBridge::default();
    let mut ui = UiApp::new().unwrap();
    assert!(bridge.sync_authority(&mut ui, &network).unwrap());
    assert_eq!(ui.snapshot.selected, 3);
    assert!(!ui.snapshot.food_known);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        assert!(!bridge.sync_authority(&mut ui, &network).unwrap());
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    let offline = Connection::new(NetworkConfig::default()).unwrap();
    assert!(bridge.sync_authority(&mut ui, &offline).unwrap());
    assert!(!ui.snapshot.connected);
    assert_eq!(ui.snapshot.inventory, [rules::stack::ItemStack::EMPTY; 41]);
    assert_eq!(ui.snapshot.selected, 0);
}
#[test]
fn network_intents_keep_authority_and_unavailable_routes_explicit() {
    let mut network = connection();
    assert!(matches!(
        ui_bridge::dispatch(&mut network, Intent::SelectSlot(4), 0).unwrap(),
        Dispatch::Sent(_)
    ));
    let outgoing = network.take_outgoing(10);
    assert!(outgoing.iter().any(|message| matches!(
        message,
        ClientMessage::Action {
            action: Action::SelectSlot(4),
            ..
        }
    )));
    let slot = Intent::SlotClick {
        window: 5,
        revision: 3,
        inventory_revision: 2,
        slot: 0,
        right: false,
        shift: false,
    };
    assert_eq!(
        ui_bridge::dispatch(&mut network, slot.clone(), 0).unwrap(),
        Dispatch::Unsupported(slot)
    );
    assert_eq!(
        ui_bridge::dispatch(&mut network, Intent::ImportPack, 0).unwrap(),
        Dispatch::Local(Intent::ImportPack)
    );
    let mut settings = Settings::default();
    settings.set(Setting::Blocks, 25).unwrap();
    settings.set(Setting::Master, 50).unwrap();
    let (master, categories) = ui_bridge::sound_levels(&settings);
    assert_eq!(master, 0.5);
    assert!(categories.contains(&(audio::Category::Machines, 0.25)));
    assert!(categories.contains(&(audio::Category::Blocks, 0.25)));
    assert!(categories.contains(&(audio::Category::Players, 1.0)));
}
