use client::{
    network::{Connection, NetworkConfig},
    ui_bridge::UiBridge,
    ui_item_facts::project,
};
use net::{
    ServerMessage,
    item_attributes::{Attribute, InventoryAttributes, ItemAttribute},
};
use rules::{item_payload::PayloadId, stack::ItemStack, survival::Inventory};
use ui::{
    app::{ItemFact, Modifiers, Screen, UiApp},
    paint::TextBuffer,
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn inventory() -> Inventory {
    let mut inventory = Inventory::default();
    inventory
        .set_trusted(
            0,
            ItemStack::new(rules::items::I_ENERGY_TABLET, 1)
                .unwrap()
                .with_payload(PayloadId(17))
                .unwrap(),
        )
        .unwrap();
    inventory
        .set_trusted(
            1,
            ItemStack::new(rules::items::I_CONFIGURATOR, 1)
                .unwrap()
                .with_payload(PayloadId(18))
                .unwrap(),
        )
        .unwrap();
    inventory
}
fn attributes(inventory: &Inventory) -> InventoryAttributes {
    InventoryAttributes {
        inventory_revision: inventory.revision(),
        rows: vec![
            ItemAttribute {
                slot: 0,
                item: rules::items::I_ENERGY_TABLET,
                payload: 17,
                attribute: Attribute::Charge(180000),
            },
            ItemAttribute {
                slot: 1,
                item: rules::items::I_CONFIGURATOR,
                payload: 18,
                attribute: Attribute::Configurator(4),
            },
        ],
    }
}
fn receive(connection: &mut Connection, message: ServerMessage) {
    let bytes = net::encode_server(&message).unwrap();
    connection
        .receive(net::decode_server(&bytes).unwrap(), 1)
        .unwrap();
}
#[test]
fn strict_owned_facts_reach_ui_and_disappear_when_inventory_revision_or_payload_changes() {
    let mut network = Connection::new(NetworkConfig::default()).unwrap();
    let mut inventory = inventory();
    let facts = attributes(&inventory);
    receive(
        &mut network,
        ServerMessage::Inventory {
            data: inventory.encode().to_vec(),
        },
    );
    receive(
        &mut network,
        ServerMessage::ItemAttributes(Box::new(facts.clone())),
    );
    let mut ui = UiApp::new().unwrap();
    let mut bridge = UiBridge::default();
    assert!(bridge.sync_authority(&mut ui, &network).unwrap());
    assert_eq!(
        ui.snapshot.inventory_facts[0],
        Some(ItemFact::Charge(180000))
    );
    assert_eq!(
        ui.snapshot.inventory_facts[1],
        Some(ItemFact::Configurator(4))
    );
    let mut text = TextBuffer::default();
    ui.snapshot.inventory_facts[0]
        .unwrap()
        .write_text(&mut text)
        .unwrap();
    assert_eq!(text.as_str(), "Заряд: 180000 / 1000000 Дж");
    assert!(!text.as_str().contains("0000000000000011"));
    text.clear();
    ui.snapshot.inventory_facts[1]
        .unwrap()
        .write_text(&mut text)
        .unwrap();
    assert_eq!(text.as_str(), "Режим: газ");
    inventory
        .set_trusted(
            1,
            ItemStack::new(rules::items::I_CONFIGURATOR, 1)
                .unwrap()
                .with_payload(PayloadId(19))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(project(Some(&facts), &inventory), [None; 41]);
    receive(
        &mut network,
        ServerMessage::Inventory {
            data: inventory.encode().to_vec(),
        },
    );
    assert!(bridge.sync_authority(&mut ui, &network).unwrap());
    assert_eq!(ui.snapshot.inventory_facts, [None; 41]);
    assert_eq!(ui.snapshot.inventory[1].payload(), Some(PayloadId(19)));
}
#[test]
fn projection_and_display_formatting_are_allocation_free_and_toolmode_is_captured_by_text_focus() {
    let inventory = inventory();
    let attributes = attributes(&inventory);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        let facts = project(Some(&attributes), &inventory);
        let mut text = TextBuffer::default();
        facts[0].unwrap().write_text(&mut text).unwrap();
        assert!(text.as_str().contains("180000"));
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    let mut ui = UiApp::new().unwrap();
    ui.set_snapshot(ui::app::Snapshot {
        connected: true,
        ..Default::default()
    })
    .unwrap();
    ui.open(Screen::Game);
    assert_eq!(
        client::ui_bridge::route_key(
            &mut ui,
            "KeyN",
            true,
            Modifiers {
                shift: true,
                ctrl: false
            }
        )
        .world,
        Some((client::view::InputKey::ToolMode, true))
    );
    client::ui_bridge::route_key(&mut ui, "KeyT", true, Modifiers::default());
    assert!(
        client::ui_bridge::route_key(&mut ui, "KeyN", true, Modifiers::default())
            .world
            .is_none()
    );
}
